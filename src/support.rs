//! What the rest of Glimt is built on: threads and their counts, text for the user, the interface's
//! languages, and what the tests and the release tests use.

pub(crate) mod format;
pub(crate) mod lang;
#[cfg(test)]
pub(crate) mod source_rules;
pub(crate) mod test_hooks;
#[cfg(test)]
pub(crate) mod test_support;
pub(crate) mod threads;
