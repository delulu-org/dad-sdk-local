//! `define_local_addon!` — the compile-time contract enforcer.
//!
//! Invoked once at the addon crate root:
//!
//! ```ignore
//! dad_local_runtime::define_local_addon! {
//!     manifest = "manifest.json";   // path relative to the addon crate root
//!     addon = OpensubsAddon;        // a type implementing Default + the
//!                                   // handler traits for declared capabilities
//! }
//! ```
//!
//! At EXPANSION time the macro:
//! 1. reads + parses `manifest.json`,
//! 2. runs the FULL dad-local-core manifest validator — an invalid manifest is
//!    a **compile error**, not a runtime surprise,
//! 3. embeds the raw JSON for the `manifest` RPC method,
//! 4. generates `fn main` (the one-shot RPC flow owned by the runtime) plus a
//!    dispatcher with arms ONLY for the declared capabilities, and
//! 5. generates the capability↔handler consistency checks in BOTH directions:
//!    - declared but missing → generated assert fails to compile
//!      ("must implement GetSubtitlesHandler…"),
//!    - implemented but undeclared → the handler trait's supertrait
//!      `HasCapability<caps::…>` is not generated for that capability, so the
//!      user's impl fails to compile ("manifest does not declare…").
//!
//! This is the Rust form of dad-sdk's `defineHttpAddon` throwing at define
//! time — except it throws at COMPILE time, before anything is ever built.

#![forbid(unsafe_code)]

use proc_macro2::{Span, TokenStream, TokenTree};
use quote::quote;
use syn::{LitStr, TypePath};

/// Parses `key = value; key = value;` pairs at top level (`;`-separated).
fn split_entries(input: TokenStream) -> Result<Vec<(String, Vec<TokenTree>)>, String> {
    let mut entries: Vec<(String, Vec<TokenTree>)> = Vec::new();
    let mut iter = input.into_iter();
    while let Some(token) = iter.next() {
        let key = match token {
            TokenTree::Ident(ident) => ident.to_string(),
            other => {
                return Err(format!(
                    "expected `manifest` or `addon`, found `{other}`"
                ))
            }
        };
        match iter.next() {
            Some(TokenTree::Punct(p)) if p.as_char() == '=' => {}
            Some(other) => return Err(format!("expected `=` after `{key}`, found `{other}`")),
            None => return Err(format!("expected `=` after `{key}`")),
        }
        let mut value: Vec<TokenTree> = Vec::new();
        for token in iter.by_ref() {
            if matches!(&token, TokenTree::Punct(p) if p.as_char() == ';') {
                break;
            }
            value.push(token);
        }
        if value.is_empty() {
            return Err(format!("missing value for `{key}`"));
        }
        entries.push((key, value));
    }
    Ok(entries)
}

fn compile_error(message: &str) -> proc_macro::TokenStream {
    let msg = format!("define_local_addon!: {message}");
    quote! { ::core::compile_error!(#msg); }.into()
}

#[proc_macro]
pub fn define_local_addon(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let manifest_dir = match std::env::var("CARGO_MANIFEST_DIR") {
        Ok(dir) => std::path::PathBuf::from(dir),
        Err(_) => {
            return compile_error("CARGO_MANIFEST_DIR not set - macros must run under cargo")
        }
    };
    match define_local_addon_impl(input.into(), &manifest_dir) {
        Ok(tokens) => tokens.into(),
        Err(message) => compile_error(&message),
    }
}

/// The testable core of the macro: parses the invocation, reads + validates
/// the manifest relative to `manifest_dir` (the addon crate root), and emits
/// the generated addon. Split out from the `#[proc_macro]` entry point so it
/// can be driven directly from unit tests (which cannot use the `proc_macro`
/// bridge) with a temporary manifest dir.
fn define_local_addon_impl(
    input: TokenStream,
    manifest_dir: &std::path::Path,
) -> Result<TokenStream, String> {
    let entries = split_entries(input)?;
    let mut manifest_lit: Option<LitStr> = None;
    let mut addon_ty: Option<TypePath> = None;
    for (key, value) in entries {
        let value_ts: TokenStream = value.into_iter().collect();
        match key.as_str() {
            "manifest" => {
                let lit: LitStr = syn::parse2(value_ts)
                    .map_err(|_| "`manifest` must be a string literal path (e.g. \"manifest.json\")".to_string())?;
                if manifest_lit.is_some() {
                    return Err("`manifest` specified more than once".to_string());
                }
                manifest_lit = Some(lit);
            }
            "addon" => {
                let ty: TypePath = syn::parse2(value_ts)
                    .map_err(|_| "`addon` must be a type path (e.g. MyAddon)".to_string())?;
                if addon_ty.is_some() {
                    return Err("`addon` specified more than once".to_string());
                }
                addon_ty = Some(ty);
            }
            other => return Err(format!("unknown entry `{other}` - expected `manifest` and `addon`")),
        }
    }
    let manifest_lit = manifest_lit.ok_or("missing `manifest = \"manifest.json\";`")?;
    let addon_ty = addon_ty.ok_or("missing `addon = MyAddon;`")?;

    // Read the manifest from disk at compile time (relative to the addon
    // crate root — CARGO_MANIFEST_DIR during macro expansion).
    let manifest_path = manifest_dir.join(manifest_lit.value());
    let manifest_json = std::fs::read_to_string(&manifest_path).map_err(|e| {
        format!(
            "failed to read manifest at {}: {e}",
            manifest_path.display()
        )
    })?;

    // FULL contract validation, at compile time. An invalid manifest breaks
    // the build with every violation listed.
    let manifest_value: serde_json::Value = serde_json::from_str(&manifest_json)
        .map_err(|e| format!("manifest is not valid JSON: {e}"))?;
    let manifest = dad_local_core::LocalAddonManifest::parse(&manifest_value)
        .map_err(|errors| {
            let mut msg = String::from("manifest.json violates the DAD local contract:\n");
            for error in &errors {
                msg.push_str(&format!("  - {error}\n"));
            }
            msg
        })?;

    let raw_json_literal = proc_macro2::Literal::string(&manifest_json);

    // Which handler surfaces does this manifest need?
    let has_streams = manifest
        .capabilities
        .iter()
        .any(|c| matches!(c, dad_local_core::DadCapability::DirectStream | dad_local_core::DadCapability::Torrent));
    let has_meta = manifest
        .capabilities
        .iter()
        .any(|c| matches!(c, dad_local_core::DadCapability::Meta));
    let has_subtitles = manifest
        .capabilities
        .iter()
        .any(|c| matches!(c, dad_local_core::DadCapability::Subtitle));

    // Compile-time asserts: DECLARED capability ⇒ handler trait implemented.
    // (The reverse direction is enforced by the traits' `HasCapability`
    // supertrait: the macro only generates the impls below for DECLARED
    // capabilities, so an undeclared handler impl fails E0277.)
    let mut asserts = Vec::new();
    if has_streams {
        asserts.push(quote! {
            #[allow(dead_code)]
            fn __dad_assert_streams<T: ::dad_local_runtime::GetStreamsHandler>() {}
            #[allow(dead_code)]
            fn __dad_check_streams() { __dad_assert_streams::<#addon_ty>(); }
        });
    }
    if has_meta {
        asserts.push(quote! {
            #[allow(dead_code)]
            fn __dad_assert_meta<T: ::dad_local_runtime::GetMetaHandler>() {}
            #[allow(dead_code)]
            fn __dad_check_meta() { __dad_assert_meta::<#addon_ty>(); }
        });
    }
    if has_subtitles {
        asserts.push(quote! {
            #[allow(dead_code)]
            fn __dad_assert_subtitles<T: ::dad_local_runtime::GetSubtitlesHandler>() {}
            #[allow(dead_code)]
            fn __dad_check_subtitles() { __dad_assert_subtitles::<#addon_ty>(); }
        });
    }

    // Capability impls for the reverse-direction check.
    let mut capability_impls = Vec::new();
    if has_streams {
        capability_impls
            .push(quote! { impl ::dad_local_runtime::HasCapability<::dad_local_runtime::caps::Streams> for #addon_ty {} });
    }
    if has_meta {
        capability_impls
            .push(quote! { impl ::dad_local_runtime::HasCapability<::dad_local_runtime::caps::Meta> for #addon_ty {} });
    }
    if has_subtitles {
        capability_impls
            .push(quote! { impl ::dad_local_runtime::HasCapability<::dad_local_runtime::caps::Subtitles> for #addon_ty {} });
    }

    // Dispatcher arms — ONLY for declared capabilities, so an undeclared
    // method answers `not_found` exactly like the HTTP router answers an
    // undeclared route.
    let mut arms = Vec::new();
    if has_streams {
        arms.push(quote! {
            "getStreams" => {
                let __req = ::dad_local_runtime::parse_request_params(__params)?;
                ::dad_local_runtime::enforce_api_key_gate(__manifest, &__req)?;
                let __items = __addon.get_streams(__req).await?;
                ::dad_local_runtime::validate_streams_or_invalid(&__items, __manifest)
            }
        });
    }
    if has_meta {
        arms.push(quote! {
            "getMeta" => {
                let __req = ::dad_local_runtime::parse_request_params(__params)?;
                ::dad_local_runtime::enforce_api_key_gate(__manifest, &__req)?;
                let __meta = __addon.get_meta(__req).await?;
                ::dad_local_runtime::validate_meta_or_invalid(__meta)
            }
        });
    }
    if has_subtitles {
        arms.push(quote! {
            "getSubtitles" => {
                let __req = ::dad_local_runtime::parse_request_params(__params)?;
                ::dad_local_runtime::enforce_api_key_gate(__manifest, &__req)?;
                let __subs = __addon.get_subtitles(__req).await?;
                ::dad_local_runtime::validate_subtitles_or_invalid(&__subs)
            }
        });
    }

    let declared_list = manifest
        .capabilities
        .iter()
        .map(|c| c.as_str())
        .collect::<Vec<_>>()
        .join(", ");

    let tokens = quote! {
        // The authoring-state manifest, embedded verbatim. The `manifest`
        // RPC method self-reports from THIS document; the host cross-checks
        // id/version/protocol_version/capabilities against the SIGNED
        // manifest at spawn time.
        const __DAD_MANIFEST_JSON: &str = #raw_json_literal;

        #(#capability_impls)*

        const _: () = {
            #(#asserts)*
        };

        fn main() {
            let __incoming = ::dad_local_runtime::read_incoming();
            if let Err(::dad_local_runtime::ProtocolFailure::EmptyInput) = __incoming {
                eprintln!(
                    "[dad-local] no request on stdin - this binary is a one-shot DAD local \
                     addon; the host must send exactly one JSON request line"
                );
                ::std::process::exit(1);
            }
            let __manifest = ::dad_local_runtime::load_embedded_manifest(__DAD_MANIFEST_JSON);
            let __addon = <#addon_ty as ::core::default::Default>::default();
            // The dispatch closure must stay `Fn` (callable per request), so
            // the async block captures REFERENCES (Copy), not the values.
            let __addon_ref = &__addon;
            let __manifest_ref = &__manifest;
            let __response = ::dad_local_runtime::handle(
                __incoming,
                &__manifest,
                |__method: ::std::string::String,
                 __params: ::core::option::Option<::serde_json::Value>| {
                    async move {
                        __dispatch(__addon_ref, __manifest_ref, __method.as_str(), __params).await
                    }
                },
            );
            ::dad_local_runtime::write_response(&__response);
        }

        async fn __dispatch(
            __addon: &#addon_ty,
            __manifest: &::dad_local_core::LocalAddonManifest,
            __method: &str,
            __params: Option<::serde_json::Value>,
        ) -> ::std::result::Result<::serde_json::Value, ::dad_local_core::DadError> {
            match __method {
                #(#arms)*
                "manifest" => ::std::result::Result::Ok(
                    ::serde_json::from_str::<::serde_json::Value>(__DAD_MANIFEST_JSON)
                        .expect("compile-time-validated manifest re-parses"),
                ),
                "healthCheck" | "health" | "ping" => ::std::result::Result::Ok(::serde_json::json!({
                    "ok": true,
                    "addon_id": __manifest.id,
                    "name": __manifest.name,
                    "version": __manifest.version,
                })),
                _ => ::std::result::Result::Err(::dad_local_core::DadError::new(
                    ::dad_local_core::DadErrorCode::NotFound,
                    format!("Unknown method '{}' - this addon declares capabilities: {}", __method, #declared_list),
                )),
            }
        }
    };

    let _ = Span::call_site(); // reserved for future span-precise diagnostics
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// A contract-valid manifest with the given capability list spliced in.
    fn valid_manifest(capabilities: &str) -> String {
        format!(
            r#"{{
  "id": "org.example.demo",
  "name": "Demo",
  "version": "0.1.0",
  "type": "local",
  "protocol_version": "2.0",
  "capabilities": [{capabilities}],
  "platform_assets": {{
    "windows-x64": {{
      "download_url": "https://example.com/demo.exe",
      "binary_name": "demo.exe",
      "sha256": "",
      "entry_command": "rpc"
    }}
  }},
  "signature": ""
}}"#
        )
    }

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dad-local-macros-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_manifest(dir: &Path, json: &str) {
        std::fs::write(dir.join("manifest.json"), json).unwrap();
    }

    /// Drives the macro's core exactly as the proc-macro entry point does, but
    /// against a caller-supplied manifest directory.
    fn expand(src: &str, dir: &Path) -> Result<String, String> {
        let input: TokenStream = src.parse().expect("test invocation must lex");
        define_local_addon_impl(input, dir).map(|tokens| tokens.to_string())
    }

    // ---- invocation parsing ------------------------------------------------

    #[test]
    fn rejects_missing_entries() {
        let dir = tmp_dir("missing");
        let err = expand("addon = Demo;", &dir).unwrap_err();
        assert!(err.contains("missing `manifest"), "{err}");

        let err = expand("manifest = \"manifest.json\";", &dir).unwrap_err();
        assert!(err.contains("missing `addon"), "{err}");
    }

    #[test]
    fn rejects_malformed_assignments() {
        let dir = tmp_dir("malformed");
        assert!(expand("manifest;", &dir).unwrap_err().contains("expected `=` after `manifest`"));
        assert!(expand("manifest = ;", &dir).unwrap_err().contains("missing value for `manifest`"));
        assert!(expand("= Demo;", &dir).unwrap_err().contains("expected `manifest` or `addon`"));
    }

    #[test]
    fn rejects_unknown_and_duplicate_entries() {
        let dir = tmp_dir("entries");
        assert!(expand("manifest = \"manifest.json\"; addon = Demo; bogus = 1;", &dir)
            .unwrap_err()
            .contains("unknown entry `bogus`"));

        let err = expand(
            "manifest = \"a.json\"; manifest = \"b.json\"; addon = Demo;",
            &dir,
        )
        .unwrap_err();
        assert!(err.contains("`manifest` specified more than once"), "{err}");
    }

    #[test]
    fn rejects_wrong_value_shapes() {
        let dir = tmp_dir("shapes");
        assert!(expand("manifest = 123; addon = Demo;", &dir)
            .unwrap_err()
            .contains("string literal path"));
        assert!(expand("manifest = \"manifest.json\"; addon = 123;", &dir)
            .unwrap_err()
            .contains("type path"));
    }

    // ---- manifest loading + validation -------------------------------------

    #[test]
    fn reports_a_missing_manifest_file() {
        let dir = tmp_dir("nofile");
        let err = expand("manifest = \"manifest.json\"; addon = Demo;", &dir).unwrap_err();
        assert!(err.contains("failed to read manifest"), "{err}");
        assert!(err.contains("manifest.json"), "{err}");
    }

    #[test]
    fn reports_invalid_json_and_contract_violations() {
        let dir = tmp_dir("invalid");
        write_manifest(&dir, "this is not json");
        assert!(expand("manifest = \"manifest.json\"; addon = Demo;", &dir)
            .unwrap_err()
            .contains("not valid JSON"));

        // A contract violation is a compile error listing every breach.
        write_manifest(&dir, &valid_manifest("\"direct_stream\"").replace("org.example.demo", "NotReverseDns"));
        let err = expand("manifest = \"manifest.json\"; addon = Demo;", &dir).unwrap_err();
        assert!(err.contains("violates the DAD local contract"), "{err}");
        assert!(err.contains("reverse-DNS"), "{err}");
    }

    // ---- generated tokens --------------------------------------------------

    #[test]
    fn emits_arms_and_bounds_only_for_declared_capabilities() {
        let dir = tmp_dir("streams-meta");
        write_manifest(&dir, &valid_manifest("\"direct_stream\", \"meta\""));
        let out = expand("manifest = \"manifest.json\"; addon = Demo;", &dir).unwrap();

        assert!(out.contains("__DAD_MANIFEST_JSON"), "manifest must be embedded verbatim");
        assert!(out.contains("\"getStreams\""), "streams arm must be generated");
        assert!(out.contains("\"getMeta\""), "meta arm must be generated");
        assert!(out.contains("__dad_check_streams"));
        assert!(out.contains("__dad_check_meta"));
        assert!(out.contains("HasCapability"));
        assert!(out.contains("Streams") && out.contains("Meta"));

        assert!(!out.contains("\"getSubtitles\""), "undeclared subtitle arm must be absent");
        assert!(!out.contains("__dad_check_subtitles"));
    }

    #[test]
    fn subtitle_only_addon_has_no_stream_arms() {
        let dir = tmp_dir("subs-only");
        write_manifest(&dir, &valid_manifest("\"subtitle\""));
        let out = expand("manifest = \"manifest.json\"; addon = Demo;", &dir).unwrap();

        assert!(out.contains("\"getSubtitles\""));
        assert!(out.contains("__dad_check_subtitles"));
        assert!(!out.contains("\"getStreams\""));
        assert!(!out.contains("\"getMeta\""));
    }

    #[test]
    fn always_emits_the_envelope_methods() {
        let dir = tmp_dir("envelope");
        write_manifest(&dir, &valid_manifest("\"direct_stream\""));
        let out = expand("manifest = \"manifest.json\"; addon = Demo;", &dir).unwrap();

        assert!(out.contains("\"manifest\""), "manifest RPC method is unconditional");
        assert!(out.contains("\"healthCheck\""), "healthCheck RPC method is unconditional");
        assert!(out.contains("\"health\""), "health RPC alias is unconditional");
        assert!(out.contains("\"ping\""), "ping RPC alias is unconditional");
        assert!(out.contains("fn main"), "the one-shot entry point must be generated");
    }
}
