//! Versioned wire-format markers.
//!
//! Every structure crush emits is tagged under the `@crush/1` namespace so that
//! (a) the format is versioned for future evolution, and (b) a single
//! `contains("@crush/")` scan detects crush's own output — the double-compression
//! / marker-injection guard ([`crate::compress::looks_crushed`]).

/// Provenance-header version token: `[@crush/1 ...]`.
pub const VERSION: &str = "@crush/1";

/// Namespace prefix shared by every marker — the double-compress guard scans for
/// this. Bump the major when the wire format changes incompatibly.
pub const NS: &str = "@crush/";

/// Columnar schema header line.
pub const COLS: &str = "@crush/1.cols";
/// Columnar dictionary table line.
pub const DICT: &str = "@crush/1.dict";
/// Tabular (JSON-array) header line.
pub const TABLE: &str = "@crush/1.table";
/// Common-path-prefix factor line.
pub const PREFIX: &str = "@crush/1.prefix=";
/// Elided-blob descriptor stem: emitted as `[@crush/1.blob N chars]`.
pub const BLOB: &str = "@crush/1.blob";
