/// Account roles.
///
/// There is exactly one privileged level. A super admin manages accounts and may
/// read every conversation belonging to a **normal** account — never another
/// super admin's, so a privileged account's own sessions stay private to it
/// (docs/architecture.md §12). Both halves of that rule live here so a second
/// privileged level cannot be introduced by editing one guard somewhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Super,
    User,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Super => "super",
            Self::User => "user",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Super => "超级管理员",
            Self::User => "普通用户",
        }
    }
}

/// Anything unrecognised falls back to the least privileged role.
pub fn parse_role(raw: Option<&str>) -> Role {
    match raw {
        Some(value) if value == Role::Super.as_str() => Role::Super,
        _ => Role::User,
    }
}

/// The principal carried by the session: a `users` row narrowed to what the
/// visibility rule and the pages need.
#[derive(Debug, Clone)]
pub struct Principal {
    pub id: i64,
    pub username: String,
    pub role: Role,
}

pub fn is_super_admin(viewer: Option<&Principal>) -> bool {
    viewer.is_some_and(|user| user.role == Role::Super)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_roles_degrade_to_the_least_privileged_one() {
        assert_eq!(parse_role(Some("super")), Role::Super);
        assert_eq!(parse_role(Some("user")), Role::User);
        for raw in [None, Some(""), Some("SUPER"), Some("root")] {
            assert_eq!(parse_role(raw), Role::User);
        }
    }

    #[test]
    fn only_the_super_role_is_privileged() {
        let user = Principal {
            id: 1,
            username: "u".to_string(),
            role: Role::User,
        };
        let admin = Principal {
            id: 2,
            username: "a".to_string(),
            role: Role::Super,
        };
        assert!(!is_super_admin(None));
        assert!(!is_super_admin(Some(&user)));
        assert!(is_super_admin(Some(&admin)));
    }
}
