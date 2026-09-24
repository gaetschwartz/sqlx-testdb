use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{Expr, ExprLit, Lit, LitStr, Meta, Token};

const FIXTURES_KEY: &str = "fixtures";
const CONFIG_KEY: &str = "config";
const MIGRATOR_KEY: &str = "migrator";
const MIGRATIONS_KEY: &str = "migrations";
const PATH_KEY: &str = "path";
const SCRIPTS_KEY: &str = "scripts";
const FIXTURES_DIR: &str = "fixtures";
const SQL_EXTENSION: &str = ".sql";

pub enum SchemaOverride {
    None,
    Migrator(syn::Path),
    Migrations(LitStr),
}

#[derive(Default)]
pub struct Args {
    pub config: Option<syn::Path>,
    pub schema: Option<SchemaOverride>,
    pub fixtures: Vec<LitStr>,
}

impl Parse for Args {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut args = Self::default();
        for meta in Punctuated::<Meta, Token![,]>::parse_terminated(input)? {
            match &meta {
                Meta::List(list) if list.path.is_ident(FIXTURES_KEY) => {
                    args.fixtures.extend(list.parse_args_with(parse_fixtures)?);
                }
                Meta::NameValue(pair) if pair.path.is_ident(CONFIG_KEY) => {
                    args.config = Some(path_of(&pair.value)?);
                }
                Meta::NameValue(pair) if pair.path.is_ident(MIGRATOR_KEY) => {
                    set_schema(&mut args, &meta, SchemaOverride::Migrator(path_of(&pair.value)?))?;
                }
                Meta::NameValue(pair) if pair.path.is_ident(MIGRATIONS_KEY) => {
                    let schema = match &pair.value {
                        Expr::Lit(ExprLit { lit: Lit::Bool(flag), .. }) if !flag.value => {
                            SchemaOverride::None
                        }
                        Expr::Lit(ExprLit { lit: Lit::Str(dir), .. }) => {
                            SchemaOverride::Migrations(dir.clone())
                        }
                        other => {
                            return Err(syn::Error::new_spanned(
                                other,
                                "expected `false` or a migrations directory",
                            ));
                        }
                    };
                    set_schema(&mut args, &meta, schema)?;
                }
                _ => {
                    return Err(syn::Error::new_spanned(
                        meta,
                        "expected `fixtures(...)`, `migrator = \"PATH\"`, \
                         `migrations = false | \"DIR\"` or `config = \"PATH\"`",
                    ));
                }
            }
        }
        if let (Some(config), Some(_)) = (&args.config, &args.schema) {
            return Err(syn::Error::new_spanned(
                config,
                "`config` already names the schema; drop `migrator`/`migrations`",
            ));
        }
        Ok(args)
    }
}

fn set_schema(args: &mut Args, meta: &Meta, schema: SchemaOverride) -> syn::Result<()> {
    if args.schema.replace(schema).is_some() {
        return Err(syn::Error::new_spanned(meta, "only one of `migrator` and `migrations`"));
    }
    Ok(())
}

fn path_of(value: &Expr) -> syn::Result<syn::Path> {
    match value {
        Expr::Lit(ExprLit { lit: Lit::Str(path), .. }) => path.parse(),
        Expr::Path(path) => Ok(path.path.clone()),
        other => Err(syn::Error::new_spanned(other, "expected a path")),
    }
}

fn parse_fixtures(input: ParseStream<'_>) -> syn::Result<Vec<LitStr>> {
    if input.peek(LitStr) {
        let names = Punctuated::<LitStr, Token![,]>::parse_terminated(input)?;
        return Ok(names.iter().map(|name| script(FIXTURES_DIR, name)).collect());
    }
    let mut dir = None;
    let mut scripts = Vec::new();
    for meta in Punctuated::<Meta, Token![,]>::parse_terminated(input)? {
        match &meta {
            Meta::NameValue(pair) if pair.path.is_ident(PATH_KEY) => match &pair.value {
                Expr::Lit(ExprLit { lit: Lit::Str(path), .. }) => dir = Some(path.value()),
                other => return Err(syn::Error::new_spanned(other, "expected a directory")),
            },
            Meta::List(list) if list.path.is_ident(SCRIPTS_KEY) => {
                let names =
                    list.parse_args_with(Punctuated::<LitStr, Token![,]>::parse_terminated)?;
                scripts.extend(names);
            }
            _ => {
                return Err(syn::Error::new_spanned(
                    meta,
                    "expected `\"name\", ...` or `path = \"DIR\", scripts(\"name\", ...)`",
                ));
            }
        }
    }
    let dir = dir.unwrap_or_else(|| FIXTURES_DIR.to_owned());
    Ok(scripts.iter().map(|name| script(&dir, name)).collect())
}

fn script(dir: &str, name: &LitStr) -> LitStr {
    let name_value = name.value();
    let path = if name_value.ends_with(SQL_EXTENSION) {
        name_value
    } else {
        let mut path =
            String::with_capacity(dir.len() + 1 + name_value.len() + SQL_EXTENSION.len());
        path.push_str(dir);
        path.push('/');
        path.push_str(&name_value);
        path.push_str(SQL_EXTENSION);
        path
    };
    LitStr::new(&path, name.span())
}
