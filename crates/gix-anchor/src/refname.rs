//! Validation of the `<target>`/`<id>` components [`crate::Store`] turns
//! into `refs/anchors/<target>/<id>` path segments.
//!
//! Both are always an [`gix::ObjectId`]'s own `to_string()` — lowercase hex,
//! 40 or 64 characters, never containing `/` or any other ref-hostile
//! character — so this check can never actually fail in practice. It is kept
//! anyway as the same defense-in-depth boundary
//! [`facet_git_tree::check_key`]-style validation takes elsewhere in this
//! stack: reject rather than trust, even when the input type appears to
//! guarantee validity. `gix` re-validates the assembled ref when it writes,
//! so this is the friendly first line, not the only one.

use crate::Error;

/// Reject a hex [`gix::ObjectId`] rendering that cannot be a Git ref-name
/// component.
///
/// `what` labels the component for the error message (`"target"` / `"id"`).
pub(crate) fn check_hex_component(what: &'static str, value: &str) -> Result<(), Error> {
    let reject = |reason: &'static str| {
        Err(Error::InvalidRefComponent {
            what,
            value: value.to_owned(),
            reason,
        })
    };

    if !matches!(value.len(), 40 | 64) {
        return reject("must be a 40- or 64-character hex object id");
    }
    if !value
        .bytes()
        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return reject("must be lowercase hex");
    }
    Ok(())
}
