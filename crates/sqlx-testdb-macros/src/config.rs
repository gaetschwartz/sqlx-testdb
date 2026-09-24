use std::path::{Path, PathBuf};

use proc_macro2::{Span, TokenStream};
use quote::quote;
use serde::Deserialize;
use syn::LitStr;

const CONFIG_FILE: &str = "sqlx-testdb.toml";
const MANIFEST_DIR_VAR: &str = "CARGO_MANIFEST_DIR";
const SECS_PER_MIN: u64 = 60;
const SECS_PER_HOUR: u64 = 60 * SECS_PER_MIN;

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Settings {
    prefix: Option<String>,
    bookkeeping_schema: Option<String>,
    database_url_var: Option<String>,
    keep_failed: Option<bool>,
    #[serde(default)]
    schema: SchemaSettings,
    #[serde(default)]
    pool: PoolSettings,
    #[serde(default)]
    sweep: SweepSettings,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct SchemaSettings {
    #[serde(default)]
    sql: Vec<String>,
    migrations: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct PoolSettings {
    max_connections: Option<u32>,
    idle_timeout_secs: Option<u64>,
    acquire_timeout_secs: Option<u64>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct SweepSettings {
    stale_run_mins: Option<u64>,
    kept_failed_hours: Option<u64>,
    idle_template_hours: Option<u64>,
}

pub struct File {
    root: String,
    path: Option<String>,
    sql: Vec<(String, String)>,
    migrations: Option<String>,
    settings: Settings,
}

fn manifest_dir(span: Span) -> syn::Result<PathBuf> {
    std::env::var_os(MANIFEST_DIR_VAR)
        .map(PathBuf::from)
        .ok_or_else(|| syn::Error::new(span, "CARGO_MANIFEST_DIR is not set; build with cargo"))
}

fn utf8(path: &Path, span: Span) -> syn::Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| syn::Error::new(span, format!("{} is not valid UTF-8", path.display())))
}

pub fn manifest_relative(dir: &LitStr) -> syn::Result<LitStr> {
    Ok(LitStr::new(&utf8(&manifest_dir(dir.span())?.join(dir.value()), dir.span())?, dir.span()))
}

impl File {
    pub fn find(span: Span) -> syn::Result<Self> {
        let manifest = manifest_dir(span)?;
        let Some(path) =
            manifest.ancestors().map(|dir| dir.join(CONFIG_FILE)).find(|p| p.is_file())
        else {
            return Ok(Self {
                root: utf8(&manifest, span)?,
                path: None,
                sql: Vec::new(),
                migrations: None,
                settings: Settings::default(),
            });
        };
        let text = std::fs::read_to_string(&path)
            .map_err(|e| syn::Error::new(span, format!("reading {}: {e}", path.display())))?;
        let mut settings: Settings = toml::from_str(&text)
            .map_err(|e| syn::Error::new(span, format!("parsing {}: {e}", path.display())))?;
        if settings.schema.migrations.is_some() && !settings.schema.sql.is_empty() {
            return Err(syn::Error::new(
                span,
                format!("{}: set `schema.sql` or `schema.migrations`, not both", path.display()),
            ));
        }
        let root = path.parent().unwrap_or(&manifest);
        let sql = std::mem::take(&mut settings.schema.sql)
            .into_iter()
            .map(|file| Ok((utf8(&root.join(&file), span)?, file)))
            .collect::<syn::Result<Vec<_>>>()?;
        let migrations =
            settings.schema.migrations.take().map(|dir| utf8(&root.join(dir), span)).transpose()?;
        Ok(Self {
            root: utf8(root, span)?,
            path: Some(utf8(&path, span)?),
            sql,
            migrations,
            settings,
        })
    }

    pub fn track(&self) -> TokenStream {
        self.path.as_ref().map_or_else(TokenStream::new, |path| {
            quote! { const _: &[u8] = ::core::include_bytes!(#path); }
        })
    }

    pub fn schema(&self) -> TokenStream {
        if let Some(dir) = &self.migrations {
            return quote! { ::sqlx_testdb::Schema::Migrations(#dir) };
        }
        if self.sql.is_empty() {
            return quote! { ::sqlx_testdb::Schema::None };
        }
        let sources = self.sql.iter().map(|(absolute, shown)| {
            quote! {
                ::sqlx_testdb::SqlSource { path: #shown, sql: ::core::include_str!(#absolute) }
            }
        });
        quote! { ::sqlx_testdb::Schema::Sql(&[#(#sources),*]) }
    }

    pub fn config(&self, schema: &TokenStream) -> TokenStream {
        let s = &self.settings;
        let root = &self.root;
        let mut fields = vec![quote! { schema: #schema }, quote! { project_root: #root }];
        if let Some(prefix) = &s.prefix {
            fields.push(quote! { prefix: #prefix });
        }
        if let Some(schema) = &s.bookkeeping_schema {
            fields.push(quote! { bookkeeping_schema: #schema });
        }
        if let Some(var) = &s.database_url_var {
            fields.push(quote! { database_url_var: #var });
        }
        if let Some(keep) = s.keep_failed {
            fields.push(quote! { keep_failed: #keep });
        }
        let mut pool = Vec::new();
        if let Some(max) = s.pool.max_connections {
            pool.push(quote! { max_connections: #max });
        }
        if let Some(secs) = s.pool.idle_timeout_secs {
            pool.push(quote! { idle_timeout: ::core::time::Duration::from_secs(#secs) });
        }
        if let Some(secs) = s.pool.acquire_timeout_secs {
            pool.push(quote! { acquire_timeout: ::core::time::Duration::from_secs(#secs) });
        }
        if !pool.is_empty() {
            fields.push(quote! {
                pool: ::sqlx_testdb::PoolSettings { #(#pool,)* ..::sqlx_testdb::PoolSettings::DEFAULT }
            });
        }
        let mut sweep = Vec::new();
        for (field, amount, unit_secs) in [
            (quote! { stale_run_after }, s.sweep.stale_run_mins, SECS_PER_MIN),
            (quote! { kept_failed_for }, s.sweep.kept_failed_hours, SECS_PER_HOUR),
            (quote! { idle_template_after }, s.sweep.idle_template_hours, SECS_PER_HOUR),
        ] {
            if let Some(amount) = amount {
                let secs = amount.saturating_mul(unit_secs);
                sweep.push(quote! { #field: ::core::time::Duration::from_secs(#secs) });
            }
        }
        if !sweep.is_empty() {
            fields.push(quote! {
                sweep: ::sqlx_testdb::SweepSettings { #(#sweep,)* ..::sqlx_testdb::SweepSettings::DEFAULT }
            });
        }
        quote! { ::sqlx_testdb::Config { #(#fields,)* ..::sqlx_testdb::Config::DEFAULT } }
    }
}
