use std::sync::LazyLock;

use sha2::{Digest, Sha256};

use crate::error::{Error, InvalidNameSnafu, NameTooLongSnafu};

const TEMPLATE_INFIX: &str = "_template_";
const BUILDING_INFIX: &str = "_building_";

const TEST_HASH_CHARS: usize = 10;
const RUN_HASH_CHARS: usize = 8;
const RANDOM_CHARS: usize = 6;
const SCHEMA_HASH_CHARS: usize = 16;
const DATABASE_SUFFIX_BYTES: usize = 1 + TEST_HASH_CHARS + 1 + RUN_HASH_CHARS + 1 + RANDOM_CHARS;
const TEMPLATE_SUFFIX_BYTES: usize = TEMPLATE_INFIX.len() + SCHEMA_HASH_CHARS;
const BUILDING_SUFFIX_BYTES: usize = BUILDING_INFIX.len() + RANDOM_CHARS;

const NEXTEST_RUN_ID_VAR: &str = "NEXTEST_RUN_ID";
const RANDOM_ALPHABET: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
const HEX: &[u8; 16] = b"0123456789abcdef";

static RUN_ID: LazyLock<String> = LazyLock::new(|| {
    std::env::var_os(NEXTEST_RUN_ID_VAR).map_or_else(
        || {
            let started = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            format!("pid-{}-{started}", std::process::id())
        },
        |id| id.to_string_lossy().into_owned(),
    )
});

#[derive(Debug, Clone, Copy)]
pub struct Names {
    pub prefix: &'static str,
    pub bookkeeping_schema: &'static str,
}

impl Names {
    pub fn validate(self, max_identifier_bytes: usize) -> Result<Self, Error> {
        for (what, name) in
            [("prefix", self.prefix), ("bookkeeping schema", self.bookkeeping_schema)]
        {
            snafu::ensure!(is_plain_identifier(name), InvalidNameSnafu { what, name });
        }
        let longest = self.prefix.len()
            + DATABASE_SUFFIX_BYTES.max(TEMPLATE_SUFFIX_BYTES).max(BUILDING_SUFFIX_BYTES);
        snafu::ensure!(
            longest <= max_identifier_bytes,
            NameTooLongSnafu {
                what: "prefix",
                name: self.prefix,
                len: longest,
                max: max_identifier_bytes
            }
        );
        snafu::ensure!(
            self.bookkeeping_schema.len() <= max_identifier_bytes,
            NameTooLongSnafu {
                what: "bookkeeping schema",
                name: self.bookkeeping_schema,
                len: self.bookkeeping_schema.len(),
                max: max_identifier_bytes,
            }
        );
        Ok(self)
    }

    pub fn template_prefix(self) -> String {
        let mut name = String::with_capacity(self.prefix.len() + TEMPLATE_INFIX.len());
        name.push_str(self.prefix);
        name.push_str(TEMPLATE_INFIX);
        name
    }

    pub fn building_prefix(self) -> String {
        let mut name = String::with_capacity(self.prefix.len() + BUILDING_INFIX.len());
        name.push_str(self.prefix);
        name.push_str(BUILDING_INFIX);
        name
    }

    pub fn template(self, schema_digest: &[u8]) -> String {
        let mut name = String::with_capacity(self.prefix.len() + TEMPLATE_SUFFIX_BYTES);
        name.push_str(self.prefix);
        name.push_str(TEMPLATE_INFIX);
        push_hex_prefix(&mut name, schema_digest, SCHEMA_HASH_CHARS);
        name
    }

    pub fn building(self) -> String {
        let mut name = String::with_capacity(self.prefix.len() + BUILDING_SUFFIX_BYTES);
        name.push_str(self.prefix);
        name.push_str(BUILDING_INFIX);
        push_random(&mut name);
        name
    }

    // <prefix>_<hash of the test path>_<run key>_<random>: finds the test, groups a run, never collides
    pub fn database(self, test_path: &str, run: &str) -> String {
        let mut name = String::with_capacity(self.prefix.len() + DATABASE_SUFFIX_BYTES);
        name.push_str(self.prefix);
        name.push('_');
        push_hex_prefix(&mut name, &Sha256::digest(test_path.as_bytes()), TEST_HASH_CHARS);
        name.push('_');
        name.push_str(run);
        name.push('_');
        push_random(&mut name);
        name
    }
}

pub fn current_run_key(project_root: &str) -> String {
    run_key(project_root, &RUN_ID)
}

pub fn run_key(project_root: &str, run_id: &str) -> String {
    let digest = Sha256::new()
        .chain_update(project_root.as_bytes())
        .chain_update([0])
        .chain_update(run_id.as_bytes())
        .finalize();
    hex_prefix(&digest, RUN_HASH_CHARS)
}

fn is_plain_identifier(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_lowercase())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn push_hex_prefix(out: &mut String, digest: &[u8], chars: usize) {
    let nibbles = digest.iter().flat_map(|&byte| [byte >> 4, byte & 0x0f]);
    out.extend(nibbles.take(chars).map(|nibble| char::from(HEX[usize::from(nibble)])));
}

fn hex_prefix(digest: &[u8], chars: usize) -> String {
    let mut out = String::with_capacity(chars);
    push_hex_prefix(&mut out, digest, chars);
    out
}

fn push_random(out: &mut String) {
    let mut bytes = [0u8; RANDOM_CHARS];
    rand::fill(&mut bytes);
    for byte in bytes {
        out.push(char::from(RANDOM_ALPHABET[usize::from(byte) % RANDOM_ALPHABET.len()]));
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    const POSTGRES_MAX_IDENTIFIER_BYTES: usize = 63;
    const NAMES: Names = Names { prefix: "tdb", bookkeeping_schema: "sqlx_testdb" };

    fn test_hash(test_path: &str) -> String {
        hex_prefix(&Sha256::digest(test_path.as_bytes()), TEST_HASH_CHARS)
    }

    fn assert_plain(name: &str, max: usize) {
        assert!(name.len() <= max, "{name} is {} bytes", name.len());
        assert!(is_plain_identifier(name), "{name}");
    }

    #[test]
    fn a_database_name_is_a_plain_identifier_of_fixed_length() {
        let run = run_key("/some/checkout", "0192f3a4-6b7c-7d8e-9f00-112233445566");
        let name = NAMES.database(&"a::very::long::module::path::".repeat(20), &run);
        assert_eq!(name.len(), NAMES.prefix.len() + DATABASE_SUFFIX_BYTES);
        assert_plain(&name, POSTGRES_MAX_IDENTIFIER_BYTES);
    }

    #[test]
    fn a_database_name_carries_the_prefix_the_test_hash_and_the_run() {
        let run = run_key("/some/checkout", "run-1");
        let name = NAMES.database("app::emails::inserts", &run);
        let parts: Vec<&str> = name.split('_').collect();
        assert_eq!(
            parts,
            ["tdb", test_hash("app::emails::inserts").as_str(), run.as_str(), parts[3]]
        );
        assert_eq!(parts[3].len(), RANDOM_CHARS);
    }

    #[test]
    fn two_names_for_the_same_test_and_run_differ() {
        let run = run_key("/some/checkout", "run-1");
        let names: HashSet<String> = (0..1000).map(|_| NAMES.database("t::same", &run)).collect();
        assert_eq!(names.len(), 1000);
        let builds: HashSet<String> = (0..1000).map(|_| NAMES.building()).collect();
        assert_eq!(builds.len(), 1000);
    }

    #[test]
    fn the_test_hash_is_stable() {
        assert_eq!(test_hash("app::emails::inserts"), "4d2a6c8512");
    }

    #[test]
    fn the_run_key_separates_projects_and_runs() {
        let a = run_key("/checkout/a", "run-1");
        assert_eq!(a, run_key("/checkout/a", "run-1"));
        assert_ne!(a, run_key("/checkout/b", "run-1"));
        assert_ne!(a, run_key("/checkout/a", "run-2"));
        assert_eq!(a.len(), RUN_HASH_CHARS);
        assert_eq!(current_run_key("/checkout/a"), current_run_key("/checkout/a"));
    }

    #[test]
    fn template_and_building_names_carry_their_prefixes() {
        let template = NAMES.template(&Sha256::digest(b"CREATE TABLE a ();"));
        assert_plain(&template, POSTGRES_MAX_IDENTIFIER_BYTES);
        assert!(template.starts_with(&NAMES.template_prefix()), "{template}");
        assert_eq!(template.len(), NAMES.prefix.len() + TEMPLATE_SUFFIX_BYTES);
        let building = NAMES.building();
        assert_plain(&building, POSTGRES_MAX_IDENTIFIER_BYTES);
        assert!(building.starts_with(&NAMES.building_prefix()), "{building}");
    }

    #[test]
    fn a_test_database_never_starts_with_the_template_or_building_prefix() {
        let run = run_key("/c", "r");
        for _ in 0..1000 {
            let name = NAMES.database("t", &run);
            assert!(!name.starts_with(&NAMES.template_prefix()), "{name}");
            assert!(!name.starts_with(&NAMES.building_prefix()), "{name}");
        }
    }

    #[test]
    fn the_longest_prefix_the_limit_allows_is_accepted_and_one_more_byte_is_not() {
        let longest = POSTGRES_MAX_IDENTIFIER_BYTES - DATABASE_SUFFIX_BYTES;
        let fits: &'static str = "p".repeat(longest).leak();
        let names =
            Names { prefix: fits, ..NAMES }.validate(POSTGRES_MAX_IDENTIFIER_BYTES).unwrap();
        assert_plain(&names.database("t", &run_key("/c", "r")), POSTGRES_MAX_IDENTIFIER_BYTES);
        let too_long: &'static str = "p".repeat(longest + 1).leak();
        let error = Names { prefix: too_long, ..NAMES }.validate(POSTGRES_MAX_IDENTIFIER_BYTES);
        assert!(matches!(error, Err(Error::NameTooLong { len: 64, max: 63, .. })), "{error:?}");
    }

    #[test]
    fn a_smaller_backend_limit_rejects_a_prefix_postgres_accepts() {
        let names = Names { prefix: "abcdefghij", ..NAMES };
        assert!(names.validate(POSTGRES_MAX_IDENTIFIER_BYTES).is_ok());
        assert!(matches!(names.validate(32), Err(Error::NameTooLong { max: 32, .. })));
    }

    #[test]
    fn names_that_would_need_quoting_are_rejected() {
        for bad in ["", "1abc", "Abc", "a-b", "a b", "a\"b", "é"] {
            let error = Names { prefix: bad.to_owned().leak(), ..NAMES }.validate(63);
            assert!(matches!(error, Err(Error::InvalidName { what: "prefix", .. })), "{bad:?}");
            let error = Names { bookkeeping_schema: bad.to_owned().leak(), ..NAMES }.validate(63);
            assert!(
                matches!(error, Err(Error::InvalidName { what: "bookkeeping schema", .. })),
                "{bad:?}"
            );
        }
    }
}
