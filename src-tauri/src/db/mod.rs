//! The metadata database: the schema layered over the recordings and every
//! query the app runs against it. Split by domain concern (CONTEXT.md); the
//! submodules re-export flat, so callers keep referring to `db::x`.

mod annotations;
mod bundle;
mod capture_day;
mod carry;
mod library;
mod scan;
mod schema;
mod timeline;
mod work;

pub use annotations::*;
pub use bundle::*;
pub use capture_day::*;
pub use carry::*;
pub use library::*;
pub use scan::*;
pub use schema::*;
pub use timeline::*;
pub use work::*;

#[cfg(test)]
mod tests;
