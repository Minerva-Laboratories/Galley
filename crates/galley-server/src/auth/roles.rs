//! Roles and their capabilities, from SPEC.md §4.3. One source of truth for every access check.
//! The top project role is `admin`. A project can have several admins (§ governance). A project
//! admin is not the same as a server admin (`users.is_admin`), who manages accounts.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Admin,
    Editor,
    Commenter,
    Viewer,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::Editor => "editor",
            Role::Commenter => "commenter",
            Role::Viewer => "viewer",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        match s {
            // `owner` is the pre-governance name for `admin`. Accept it so old data and links resolve.
            "admin" | "owner" => Some(Role::Admin),
            "editor" => Some(Role::Editor),
            "commenter" => Some(Role::Commenter),
            "viewer" => Some(Role::Viewer),
            _ => None,
        }
    }

    /// Edit the text (admin, editor).
    pub fn can_edit(self) -> bool {
        matches!(self, Role::Admin | Role::Editor)
    }

    /// Leave comments and suggestions. Every role except a plain viewer can do this.
    pub fn can_comment(self) -> bool {
        matches!(self, Role::Admin | Role::Editor | Role::Commenter)
    }

    pub fn can_suggest(self) -> bool {
        self.can_comment()
    }

    /// Trigger a compile and see the PDF. Commenters may do this (spec §4.3: "✓ read PDF").
    /// Viewers may not.
    pub fn can_compile(self) -> bool {
        matches!(self, Role::Admin | Role::Editor | Role::Commenter)
    }

    /// Run agents (admin, editor).
    pub fn can_agents(self) -> bool {
        self.can_edit()
    }

    /// See files, history, and the last PDF (any member).
    pub fn can_view(self) -> bool {
        true
    }

    /// Manage members, share links, and history reverts. This is a project admin.
    pub fn is_admin(self) -> bool {
        self == Role::Admin
    }

    /// Roles a share link may grant. Admin is never handed out via a link.
    pub fn shareable() -> &'static [Role] {
        &[Role::Editor, Role::Commenter, Role::Viewer]
    }

    /// A higher rank means a more capable role. This keeps the stronger of two roles.
    pub fn rank(self) -> u8 {
        match self {
            Role::Viewer => 0,
            Role::Commenter => 1,
            Role::Editor => 2,
            Role::Admin => 3,
        }
    }

    /// The more capable of two roles.
    pub fn max(self, other: Role) -> Role {
        if self.rank() >= other.rank() {
            self
        } else {
            other
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_matrix_matches_spec() {
        assert!(Role::Admin.can_edit() && Role::Editor.can_edit());
        assert!(!Role::Commenter.can_edit() && !Role::Viewer.can_edit());
        assert!(Role::Commenter.can_comment() && Role::Commenter.can_compile());
        assert!(!Role::Viewer.can_comment() && !Role::Viewer.can_compile());
        assert!(Role::Admin.can_agents() && !Role::Commenter.can_agents());
        assert!(Role::Admin.is_admin() && !Role::Editor.is_admin());
        assert!(Role::Viewer.can_view());
    }

    #[test]
    fn round_trips_and_accepts_legacy_owner() {
        for r in [Role::Admin, Role::Editor, Role::Commenter, Role::Viewer] {
            assert_eq!(Role::parse(r.as_str()), Some(r));
        }
        assert_eq!(Role::parse("owner"), Some(Role::Admin));
        assert_eq!(Role::parse("nope"), None);
    }
}
