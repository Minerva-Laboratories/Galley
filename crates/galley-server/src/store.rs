//! All SQLite reads and writes. The methods are synchronous because rusqlite blocks. Call them
//! from `spawn_blocking`. The `Db` handle inside is a pool that is cheap to clone.

use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};
use serde::Serialize;

use crate::auth::{hash_password, new_id, token_hash, Role};
use crate::db::{Db, DbError};

#[derive(Clone)]
pub struct Store {
    db: Db,
}

#[derive(Debug, Clone, Serialize)]
pub struct User {
    pub id: String,
    pub email: String,
    pub name: String,
    pub is_admin: bool,
    pub is_guest: bool,
    #[serde(skip)]
    pub pw_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Member {
    pub user_id: String,
    pub name: String,
    pub email: String,
    pub role: Role,
    pub is_guest: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ShareLink {
    pub id: String,
    pub project_id: String,
    pub role: Role,
    pub label: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub revoked: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Comment {
    pub id: String,
    pub file: String,
    pub anchor: String,
    pub quote: Option<String>,
    pub author_id: String,
    pub author_name: String,
    pub body: String,
    pub resolved: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Suggestion {
    pub id: String,
    pub file: String,
    pub anchor: String,
    pub anchor_end: String,
    pub quote: String,
    pub replacement: String,
    pub author_id: String,
    pub author_name: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Checkpoint {
    pub id: String,
    pub commit_sha: String,
    pub short_sha: String,
    pub label: String,
    pub author_name: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceToken {
    /// The token's hash is also its id. The server shows the token once, at creation.
    pub id: String,
    pub label: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentRun {
    pub id: String,
    pub agent: String,
    pub model: Option<String>,
    pub summary: String,
    pub edits: i64,
    pub author_name: String,
    pub created_at: DateTime<Utc>,
}

/// How role changes are authorized on a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GovernanceMode {
    /// Any admin acts alone (the default).
    Solo,
    /// More than half of the admins must approve.
    Majority,
    /// Every admin must approve.
    Unanimous,
}

impl GovernanceMode {
    pub fn as_str(self) -> &'static str {
        match self {
            GovernanceMode::Solo => "solo",
            GovernanceMode::Majority => "majority",
            GovernanceMode::Unanimous => "unanimous",
        }
    }

    pub fn parse(s: &str) -> Option<GovernanceMode> {
        match s {
            "solo" => Some(GovernanceMode::Solo),
            "majority" => Some(GovernanceMode::Majority),
            "unanimous" => Some(GovernanceMode::Unanimous),
            _ => None,
        }
    }

    /// Decide from the tally over the current electorate whether a request passes, fails, or
    /// keeps waiting.
    pub fn evaluate(self, electorate: usize, approvals: usize, rejects: usize) -> Decision {
        if electorate == 0 {
            return Decision::Pending;
        }
        match self {
            GovernanceMode::Solo => Decision::Pass, // solo never creates a request
            GovernanceMode::Unanimous => {
                if rejects > 0 {
                    Decision::Fail
                } else if approvals >= electorate {
                    Decision::Pass
                } else {
                    Decision::Pending
                }
            }
            GovernanceMode::Majority => {
                if approvals * 2 > electorate {
                    Decision::Pass
                } else if rejects * 2 >= electorate {
                    Decision::Fail
                } else {
                    Decision::Pending
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Pass,
    Fail,
    Pending,
}

/// The outcome of evaluating a pending request against the current admins and mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    Applied,
    Rejected,
    Pending,
    /// The request is closed or no longer exists.
    Gone,
}

#[derive(Debug, Clone, Serialize)]
pub struct VoteRecord {
    pub admin_id: String,
    pub admin_name: String,
    pub vote: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RoleRequest {
    pub id: String,
    pub kind: String,
    pub target_id: Option<String>,
    pub payload: serde_json::Value,
    pub summary: String,
    pub proposer_id: String,
    pub proposer_name: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub votes: Vec<VoteRecord>,
}

impl Store {
    pub fn new(db: Db) -> Store {
        Store { db }
    }

    // --- users & sessions ------------------------------------------------------------------

    pub fn user_count(&self) -> Result<i64, DbError> {
        let conn = self.db.conn()?;
        Ok(conn.query_row("SELECT COUNT(*) FROM users WHERE is_guest = 0", [], |r| r.get(0))?)
    }

    pub fn create_user(&self, email: &str, name: &str, password: &str, is_admin: bool) -> Result<User, StoreError> {
        let email = email.trim().to_lowercase();
        let name = name.trim();
        if name.is_empty() {
            return Err(StoreError::Invalid("A name is required.".into()));
        }
        if password.len() < 8 {
            return Err(StoreError::Invalid("Passwords must be at least 8 characters.".into()));
        }
        let hash = hash_password(password).map_err(|e| StoreError::Invalid(e.to_string()))?;
        let id = new_id();
        let conn = self.db.conn()?;
        conn.execute(
            "INSERT INTO users (id, email, name, pw_hash, is_admin, is_guest, created_at) VALUES (?1,?2,?3,?4,?5,0,?6)",
            params![id, email, name, hash, is_admin as i64, Utc::now()],
        )
        .map_err(|e| match e {
            rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation => {
                StoreError::Conflict("An account with that email already exists.".into())
            }
            other => StoreError::Db(other.into()),
        })?;
        Ok(User {
            id,
            email,
            name: name.to_string(),
            is_admin,
            is_guest: false,
            pw_hash: Some(hash),
        })
    }

    /// A throwaway account for a share-link visitor, identified only by a display name.
    pub fn create_guest(&self, name: &str) -> Result<User, StoreError> {
        let name = name.trim();
        let name = if name.is_empty() { "Anonymous" } else { name };
        let id = new_id();
        let conn = self.db.conn()?;
        conn.execute(
            "INSERT INTO users (id, email, name, pw_hash, is_admin, is_guest, created_at) VALUES (?1,'',?2,NULL,0,1,?3)",
            params![id, name, Utc::now()],
        )?;
        Ok(User {
            id,
            email: String::new(),
            name: name.to_string(),
            is_admin: false,
            is_guest: true,
            pw_hash: None,
        })
    }

    pub fn user_by_email(&self, email: &str) -> Result<Option<User>, DbError> {
        let conn = self.db.conn()?;
        conn.query_row(
            "SELECT id, email, name, pw_hash, is_admin, is_guest FROM users WHERE email = ?1 AND is_guest = 0",
            params![email.trim().to_lowercase()],
            row_to_user,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn reset_password(&self, user_id: &str, password: &str) -> Result<(), StoreError> {
        if password.len() < 8 {
            return Err(StoreError::Invalid("Passwords must be at least 8 characters.".into()));
        }
        let hash = hash_password(password).map_err(|e| StoreError::Invalid(e.to_string()))?;
        let conn = self.db.conn()?;
        let n = conn.execute("UPDATE users SET pw_hash = ?2 WHERE id = ?1 AND is_guest = 0", params![user_id, hash])?;
        if n == 0 {
            return Err(StoreError::NotFound("No such account.".into()));
        }
        // Signing keys change: drop this user's sessions so the old cookie stops working.
        conn.execute("DELETE FROM sessions WHERE user_id = ?1", params![user_id])?;
        Ok(())
    }

    pub fn list_users(&self) -> Result<Vec<User>, DbError> {
        let conn = self.db.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, email, name, pw_hash, is_admin, is_guest FROM users WHERE is_guest = 0 ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], row_to_user)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn list_admins(&self) -> Result<Vec<User>, DbError> {
        Ok(self.list_users()?.into_iter().filter(|u| u.is_admin).collect())
    }

    pub fn user_by_id(&self, id: &str) -> Result<Option<User>, DbError> {
        let conn = self.db.conn()?;
        conn.query_row(
            "SELECT id, email, name, pw_hash, is_admin, is_guest FROM users WHERE id = ?1",
            params![id],
            row_to_user,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Create a session and return its cookie value. The value is the raw token. The store keeps
    /// only its hash.
    pub fn create_session(&self, user_id: &str, days: i64) -> Result<String, DbError> {
        let token = crate::auth::random_token();
        let conn = self.db.conn()?;
        conn.execute(
            "INSERT INTO sessions (token_hash, user_id, created_at, expires_at) VALUES (?1,?2,?3,?4)",
            params![token_hash(&token), user_id, Utc::now(), Utc::now() + chrono::Duration::days(days)],
        )?;
        Ok(token)
    }

    pub fn session_user(&self, token: &str) -> Result<Option<User>, DbError> {
        let conn = self.db.conn()?;
        let user = conn
            .query_row(
                "SELECT u.id, u.email, u.name, u.pw_hash, u.is_admin, u.is_guest
                 FROM sessions s JOIN users u ON u.id = s.user_id
                 WHERE s.token_hash = ?1 AND s.expires_at > ?2",
                params![token_hash(token), Utc::now()],
                row_to_user,
            )
            .optional()?;
        Ok(user)
    }

    pub fn delete_session(&self, token: &str) -> Result<(), DbError> {
        let conn = self.db.conn()?;
        conn.execute("DELETE FROM sessions WHERE token_hash = ?1", params![token_hash(token)])?;
        Ok(())
    }

    // --- membership ------------------------------------------------------------------------

    pub fn set_member(&self, project_id: &str, user_id: &str, role: Role) -> Result<(), DbError> {
        let conn = self.db.conn()?;
        conn.execute(
            "INSERT INTO members (project_id, user_id, role, created_at) VALUES (?1,?2,?3,?4)
             ON CONFLICT(project_id, user_id) DO UPDATE SET role = excluded.role",
            params![project_id, user_id, role.as_str(), Utc::now()],
        )?;
        Ok(())
    }

    pub fn remove_member(&self, project_id: &str, user_id: &str) -> Result<(), DbError> {
        let conn = self.db.conn()?;
        conn.execute("DELETE FROM members WHERE project_id = ?1 AND user_id = ?2", params![project_id, user_id])?;
        Ok(())
    }

    pub fn role_of(&self, project_id: &str, user_id: &str) -> Result<Option<Role>, DbError> {
        let conn = self.db.conn()?;
        let role: Option<String> = conn
            .query_row(
                "SELECT role FROM members WHERE project_id = ?1 AND user_id = ?2",
                params![project_id, user_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(role.and_then(|r| Role::parse(&r)))
    }

    pub fn members(&self, project_id: &str) -> Result<Vec<Member>, DbError> {
        let conn = self.db.conn()?;
        let mut stmt = conn.prepare(
            "SELECT m.user_id, u.name, u.email, m.role, u.is_guest FROM members m
             JOIN users u ON u.id = m.user_id WHERE m.project_id = ?1
             ORDER BY CASE m.role WHEN 'admin' THEN 0 WHEN 'editor' THEN 1 WHEN 'commenter' THEN 2 ELSE 3 END, u.name",
        )?;
        let rows = stmt
            .query_map(params![project_id], |r| {
                Ok(Member {
                    user_id: r.get(0)?,
                    name: r.get(1)?,
                    email: r.get(2)?,
                    role: Role::parse(&r.get::<_, String>(3)?).unwrap_or(Role::Viewer),
                    is_guest: r.get::<_, i64>(4)? != 0,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Project ids the user owns, in no particular order. The registry sorts them.
    pub fn owned_projects(&self, user_id: &str) -> Result<Vec<String>, DbError> {
        let conn = self.db.conn()?;
        let mut stmt = conn.prepare("SELECT project_id FROM members WHERE user_id = ?1 AND role = 'admin'")?;
        let rows = stmt.query_map(params![user_id], |r| r.get(0))?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn project_ids_for(&self, user_id: &str) -> Result<Vec<(String, Role)>, DbError> {
        let conn = self.db.conn()?;
        let mut stmt = conn.prepare("SELECT project_id, role FROM members WHERE user_id = ?1")?;
        let rows = stmt
            .query_map(params![user_id], |r| {
                Ok((r.get::<_, String>(0)?, Role::parse(&r.get::<_, String>(1)?).unwrap_or(Role::Viewer)))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn invite_by_email(&self, project_id: &str, email: &str, role: Role) -> Result<Member, StoreError> {
        let user = self
            .user_by_email(email)?
            .ok_or_else(|| StoreError::NotFound(format!("No account for {email}. They need to sign in once before you can add them.")))?;
        self.set_member(project_id, &user.id, role)?;
        Ok(Member {
            user_id: user.id,
            name: user.name,
            email: user.email,
            role,
            is_guest: user.is_guest,
        })
    }

    // --- share links -----------------------------------------------------------------------

    /// Create a share link, returning (stored row, raw token). The token is shown once.
    pub fn create_share(
        &self,
        project_id: &str,
        role: Role,
        label: Option<&str>,
        expires_at: Option<DateTime<Utc>>,
        created_by: &str,
    ) -> Result<(ShareLink, String), DbError> {
        let token = crate::auth::random_token();
        let id = new_id();
        let now = Utc::now();
        let conn = self.db.conn()?;
        conn.execute(
            "INSERT INTO share_links (id, project_id, role, token_hash, label, expires_at, created_by, created_at, revoked)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,0)",
            params![id, project_id, role.as_str(), token_hash(&token), label, expires_at, created_by, now],
        )?;
        Ok((
            ShareLink {
                id,
                project_id: project_id.to_string(),
                role,
                label: label.map(str::to_string),
                expires_at,
                created_at: now,
                revoked: false,
            },
            token,
        ))
    }

    pub fn shares(&self, project_id: &str) -> Result<Vec<ShareLink>, DbError> {
        let conn = self.db.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, project_id, role, label, expires_at, created_at, revoked FROM share_links
             WHERE project_id = ?1 AND revoked = 0 ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(params![project_id], row_to_share)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn revoke_share(&self, project_id: &str, id: &str) -> Result<bool, DbError> {
        let conn = self.db.conn()?;
        let n = conn.execute(
            "UPDATE share_links SET revoked = 1 WHERE id = ?1 AND project_id = ?2",
            params![id, project_id],
        )?;
        Ok(n > 0)
    }

    // --- checkpoints ------------------------------------------------------------------------

    /// Generate a ref-safe checkpoint id and record the row. The caller creates the matching git
    /// tag (`galley/checkpoint/<id>`) and fills in `commit_sha` from it.
    pub fn new_checkpoint_id(&self) -> String {
        new_id()
    }

    pub fn record_checkpoint(
        &self,
        id: &str,
        project_id: &str,
        commit_sha: &str,
        label: &str,
        author_id: &str,
    ) -> Result<Checkpoint, DbError> {
        let now = Utc::now();
        let conn = self.db.conn()?;
        conn.execute(
            "INSERT INTO checkpoints (id, project_id, commit_sha, label, author_id, created_at)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![id, project_id, commit_sha, label, author_id, now],
        )?;
        Ok(Checkpoint {
            id: id.to_string(),
            short_sha: commit_sha.chars().take(7).collect(),
            commit_sha: commit_sha.to_string(),
            label: label.to_string(),
            author_name: self.user_by_id(author_id)?.map(|u| u.name),
            created_at: now,
        })
    }

    pub fn checkpoints(&self, project_id: &str) -> Result<Vec<Checkpoint>, DbError> {
        let conn = self.db.conn()?;
        let mut stmt = conn.prepare(
            "SELECT c.id, c.commit_sha, c.label, u.name, c.created_at
             FROM checkpoints c LEFT JOIN users u ON u.id = c.author_id
             WHERE c.project_id = ?1 ORDER BY c.created_at DESC",
        )?;
        let rows = stmt
            .query_map(params![project_id], |r| {
                let commit_sha: String = r.get(1)?;
                Ok(Checkpoint {
                    id: r.get(0)?,
                    short_sha: commit_sha.chars().take(7).collect(),
                    commit_sha,
                    label: r.get(2)?,
                    author_name: r.get(3)?,
                    created_at: r.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Resolve a share token to (project_id, role) if it is live.
    pub fn share_by_token(&self, token: &str) -> Result<Option<(String, Role)>, DbError> {
        let conn = self.db.conn()?;
        let row = conn
            .query_row(
                "SELECT project_id, role, expires_at FROM share_links
                 WHERE token_hash = ?1 AND revoked = 0",
                params![token_hash(token)],
                |r| {
                    let project: String = r.get(0)?;
                    let role: String = r.get(1)?;
                    let expires: Option<DateTime<Utc>> = r.get(2)?;
                    Ok((project, role, expires))
                },
            )
            .optional()?;
        Ok(match row {
            Some((p, role, expires)) if expires.is_none_or(|e| e > Utc::now()) => {
                Role::parse(&role).map(|r| (p, r))
            }
            _ => None,
        })
    }

    // --- comments --------------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn add_comment(
        &self,
        project_id: &str,
        file: &str,
        anchor: &str,
        quote: Option<&str>,
        author: &User,
        body: &str,
    ) -> Result<Comment, StoreError> {
        let body = body.trim();
        if body.is_empty() {
            return Err(StoreError::Invalid("A comment cannot be empty.".into()));
        }
        let id = new_id();
        let now = Utc::now();
        let conn = self.db.conn()?;
        conn.execute(
            "INSERT INTO comments (id, project_id, file, anchor, quote, author_id, author_name, body, resolved, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,0,?9)",
            params![id, project_id, file, anchor, quote, author.id, author.name, body, now],
        )?;
        Ok(Comment {
            id,
            file: file.to_string(),
            anchor: anchor.to_string(),
            quote: quote.map(str::to_string),
            author_id: author.id.clone(),
            author_name: author.name.clone(),
            body: body.to_string(),
            resolved: false,
            created_at: now,
        })
    }

    pub fn comments(&self, project_id: &str) -> Result<Vec<Comment>, DbError> {
        let conn = self.db.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, file, anchor, quote, author_id, author_name, body, resolved, created_at
             FROM comments WHERE project_id = ?1 ORDER BY created_at",
        )?;
        let rows = stmt.query_map(params![project_id], row_to_comment)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn resolve_comment(&self, project_id: &str, id: &str, resolved: bool) -> Result<bool, DbError> {
        let conn = self.db.conn()?;
        let n = conn.execute(
            "UPDATE comments SET resolved = ?3 WHERE id = ?1 AND project_id = ?2",
            params![id, project_id, resolved as i64],
        )?;
        Ok(n > 0)
    }

    pub fn comment_author(&self, project_id: &str, id: &str) -> Result<Option<String>, DbError> {
        let conn = self.db.conn()?;
        conn.query_row(
            "SELECT author_id FROM comments WHERE id = ?1 AND project_id = ?2",
            params![id, project_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(Into::into)
    }

    // --- suggestions -----------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn add_suggestion(
        &self,
        project_id: &str,
        file: &str,
        anchor: &str,
        anchor_end: &str,
        quote: &str,
        replacement: &str,
        author: &User,
    ) -> Result<Suggestion, StoreError> {
        if quote.is_empty() && replacement.is_empty() {
            return Err(StoreError::Invalid("A suggestion needs some text.".into()));
        }
        let id = new_id();
        let now = Utc::now();
        let conn = self.db.conn()?;
        conn.execute(
            "INSERT INTO suggestions (id, project_id, file, anchor, anchor_end, quote, replacement, author_id, author_name, status, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,'open',?10)",
            params![id, project_id, file, anchor, anchor_end, quote, replacement, author.id, author.name, now],
        )?;
        Ok(Suggestion {
            id,
            file: file.to_string(),
            anchor: anchor.to_string(),
            anchor_end: anchor_end.to_string(),
            quote: quote.to_string(),
            replacement: replacement.to_string(),
            author_id: author.id.clone(),
            author_name: author.name.clone(),
            status: "open".to_string(),
            created_at: now,
        })
    }

    pub fn suggestions(&self, project_id: &str) -> Result<Vec<Suggestion>, DbError> {
        let conn = self.db.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, file, anchor, anchor_end, quote, replacement, author_id, author_name, status, created_at
             FROM suggestions WHERE project_id = ?1 ORDER BY created_at",
        )?;
        let rows = stmt.query_map(params![project_id], row_to_suggestion)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn set_suggestion_status(&self, project_id: &str, id: &str, status: &str) -> Result<bool, DbError> {
        let conn = self.db.conn()?;
        let n = conn.execute(
            "UPDATE suggestions SET status = ?3 WHERE id = ?1 AND project_id = ?2 AND status = 'open'",
            params![id, project_id, status],
        )?;
        Ok(n > 0)
    }

    // --- governance & role-change requests -------------------------------------------------

    pub fn governance(&self, project_id: &str) -> Result<GovernanceMode, DbError> {
        let conn = self.db.conn()?;
        let mode: Option<String> = conn
            .query_row("SELECT governance FROM project_config WHERE project_id = ?1", params![project_id], |r| r.get(0))
            .optional()?;
        Ok(mode.and_then(|m| GovernanceMode::parse(&m)).unwrap_or(GovernanceMode::Solo))
    }

    pub fn set_governance_now(&self, project_id: &str, mode: GovernanceMode) -> Result<(), DbError> {
        let conn = self.db.conn()?;
        conn.execute(
            "INSERT INTO project_config (project_id, governance) VALUES (?1, ?2)
             ON CONFLICT(project_id) DO UPDATE SET governance = excluded.governance",
            params![project_id, mode.as_str()],
        )?;
        Ok(())
    }

    /// The electorate for governed changes. It holds the project admins who are real accounts.
    /// It never holds guests.
    pub fn admin_ids(&self, project_id: &str) -> Result<Vec<String>, DbError> {
        let conn = self.db.conn()?;
        let mut stmt = conn.prepare(
            "SELECT m.user_id FROM members m JOIN users u ON u.id = m.user_id
             WHERE m.project_id = ?1 AND m.role = 'admin' AND u.is_guest = 0",
        )?;
        let rows = stmt.query_map(params![project_id], |r| r.get(0))?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Record a pending request and the proposer's own approval. Returns it with its votes.
    pub fn create_request(
        &self,
        project_id: &str,
        kind: &str,
        target_id: Option<&str>,
        payload: &serde_json::Value,
        summary: &str,
        proposer: &User,
    ) -> Result<RoleRequest, StoreError> {
        let id = new_id();
        let now = Utc::now();
        let conn = self.db.conn()?;
        conn.execute(
            "INSERT INTO role_requests (id, project_id, kind, target_id, payload, summary, proposer_id, proposer_name, status, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'open',?9)",
            params![id, project_id, kind, target_id, payload.to_string(), summary, proposer.id, proposer.name, now],
        )?;
        conn.execute(
            "INSERT INTO role_request_votes (request_id, admin_id, vote, created_at) VALUES (?1,?2,'approve',?3)",
            params![id, proposer.id, now],
        )?;
        drop(conn);
        self.request(project_id, &id)?.ok_or_else(|| StoreError::NotFound("request vanished".into()))
    }

    pub fn request(&self, project_id: &str, id: &str) -> Result<Option<RoleRequest>, DbError> {
        let conn = self.db.conn()?;
        let base = conn
            .query_row(
                "SELECT id, kind, target_id, payload, summary, proposer_id, proposer_name, status, created_at
                 FROM role_requests WHERE id = ?1 AND project_id = ?2",
                params![id, project_id],
                row_to_request,
            )
            .optional()?;
        let Some(mut req) = base else { return Ok(None) };
        let mut stmt = conn.prepare(
            "SELECT v.admin_id, u.name, v.vote FROM role_request_votes v JOIN users u ON u.id = v.admin_id
             WHERE v.request_id = ?1",
        )?;
        req.votes = stmt
            .query_map(params![id], |r| {
                Ok(VoteRecord { admin_id: r.get(0)?, admin_name: r.get(1)?, vote: r.get(2)? })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(req))
    }

    pub fn open_requests(&self, project_id: &str) -> Result<Vec<RoleRequest>, DbError> {
        let ids: Vec<String> = {
            let conn = self.db.conn()?;
            let mut stmt = conn.prepare(
                "SELECT id FROM role_requests WHERE project_id = ?1 AND status = 'open' ORDER BY created_at",
            )?;
            let v = stmt.query_map(params![project_id], |r| r.get(0))?.collect::<Result<Vec<_>, _>>()?;
            v
        };
        let mut out = Vec::new();
        for id in ids {
            if let Some(req) = self.request(project_id, &id)? {
                out.push(req);
            }
        }
        Ok(out)
    }

    pub fn record_vote(&self, request_id: &str, admin_id: &str, approve: bool) -> Result<(), DbError> {
        let conn = self.db.conn()?;
        conn.execute(
            "INSERT INTO role_request_votes (request_id, admin_id, vote, created_at) VALUES (?1,?2,?3,?4)
             ON CONFLICT(request_id, admin_id) DO UPDATE SET vote = excluded.vote, created_at = excluded.created_at",
            params![request_id, admin_id, if approve { "approve" } else { "reject" }, Utc::now()],
        )?;
        Ok(())
    }

    pub fn set_request_status(&self, id: &str, status: &str) -> Result<(), DbError> {
        let conn = self.db.conn()?;
        conn.execute(
            "UPDATE role_requests SET status = ?2, resolved_at = ?3 WHERE id = ?1 AND status = 'open'",
            params![id, status, Utc::now()],
        )?;
        Ok(())
    }

    /// Apply a request's action. This method stays small. The callers decide when the threshold
    /// is met.
    pub fn apply_request(&self, project_id: &str, req: &RoleRequest) -> Result<(), StoreError> {
        match req.kind.as_str() {
            "set_role" => {
                let role = req
                    .payload
                    .get("role")
                    .and_then(|v| v.as_str())
                    .and_then(Role::parse)
                    .ok_or_else(|| StoreError::Invalid("bad role in request".into()))?;
                let target = req.target_id.as_deref().ok_or_else(|| StoreError::Invalid("no target".into()))?;
                self.set_member(project_id, target, role)?;
            }
            "remove_member" => {
                let target = req.target_id.as_deref().ok_or_else(|| StoreError::Invalid("no target".into()))?;
                self.remove_member(project_id, target)?;
            }
            "invite" => {
                let email = req.payload.get("email").and_then(|v| v.as_str()).unwrap_or("");
                let role = req
                    .payload
                    .get("role")
                    .and_then(|v| v.as_str())
                    .and_then(Role::parse)
                    .ok_or_else(|| StoreError::Invalid("bad role".into()))?;
                self.invite_by_email(project_id, email, role)?;
            }
            "set_governance" => {
                let mode = req
                    .payload
                    .get("governance")
                    .and_then(|v| v.as_str())
                    .and_then(GovernanceMode::parse)
                    .ok_or_else(|| StoreError::Invalid("bad mode".into()))?;
                self.set_governance_now(project_id, mode)?;
            }
            other => return Err(StoreError::Invalid(format!("unknown request kind {other}"))),
        }
        Ok(())
    }

    /// Tally the votes of the *current* admins against the *current* mode. Then apply, reject or wait.
    pub fn evaluate_request(&self, project_id: &str, request_id: &str) -> Result<Resolution, StoreError> {
        let Some(req) = self.request(project_id, request_id)? else {
            return Ok(Resolution::Gone);
        };
        if req.status != "open" {
            return Ok(Resolution::Gone);
        }
        let electorate: std::collections::HashSet<String> = self.admin_ids(project_id)?.into_iter().collect();
        let mut approvals = 0;
        let mut rejects = 0;
        for v in &req.votes {
            if !electorate.contains(&v.admin_id) {
                continue; // a vote from someone no longer an admin does not count
            }
            if v.vote == "approve" {
                approvals += 1;
            } else {
                rejects += 1;
            }
        }
        let mode = self.governance(project_id)?;
        match mode.evaluate(electorate.len(), approvals, rejects) {
            Decision::Pass => {
                self.apply_request(project_id, &req)?;
                self.set_request_status(&req.id, "applied")?;
                Ok(Resolution::Applied)
            }
            Decision::Fail => {
                self.set_request_status(&req.id, "rejected")?;
                Ok(Resolution::Rejected)
            }
            Decision::Pending => Ok(Resolution::Pending),
        }
    }


    // --- device tokens (MCP clients, runners) -------------------------------------------

    /// Mint a token for `user_id`. The server returns it once and stores only its hash.
    pub fn create_device_token(&self, user_id: &str, label: &str) -> Result<(DeviceToken, String), DbError> {
        let token = crate::auth::random_token();
        let hash = token_hash(&token);
        let now = Utc::now();
        let conn = self.db.conn()?;
        conn.execute(
            "INSERT INTO device_tokens (token_hash, user_id, label, created_at, revoked) VALUES (?1,?2,?3,?4,0)",
            params![hash, user_id, label, now],
        )?;
        Ok((DeviceToken { id: hash, label: label.to_string(), created_at: now, last_used_at: None }, token))
    }

    /// The user behind a bearer token. This method touches `last_used_at`. It returns `None` if
    /// the token is unknown or revoked.
    pub fn device_token_user(&self, token: &str) -> Result<Option<User>, DbError> {
        let hash = token_hash(token);
        let conn = self.db.conn()?;
        let user_id: Option<String> = conn
            .query_row(
                "SELECT user_id FROM device_tokens WHERE token_hash = ?1 AND revoked = 0",
                params![hash],
                |r| r.get(0),
            )
            .optional()?;
        let Some(user_id) = user_id else { return Ok(None) };
        conn.execute("UPDATE device_tokens SET last_used_at = ?2 WHERE token_hash = ?1", params![hash, Utc::now()])?;
        self.user_by_id(&user_id)
    }

    pub fn device_tokens(&self, user_id: &str) -> Result<Vec<DeviceToken>, DbError> {
        let conn = self.db.conn()?;
        let mut stmt = conn.prepare(
            "SELECT token_hash, label, created_at, last_used_at FROM device_tokens
             WHERE user_id = ?1 AND revoked = 0 ORDER BY created_at DESC",
        )?;
        let rows = stmt
            .query_map(params![user_id], |r| {
                Ok(DeviceToken { id: r.get(0)?, label: r.get(1)?, created_at: r.get(2)?, last_used_at: r.get(3)? })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn revoke_device_token(&self, user_id: &str, id: &str) -> Result<bool, DbError> {
        let conn = self.db.conn()?;
        let n = conn.execute(
            "UPDATE device_tokens SET revoked = 1 WHERE token_hash = ?1 AND user_id = ?2",
            params![id, user_id],
        )?;
        Ok(n > 0)
    }

    // --- agent runs -----------------------------------------------------------------------

    pub fn record_agent_run(
        &self,
        project_id: &str,
        user: &User,
        agent: &str,
        model: Option<&str>,
        summary: &str,
        edits: i64,
    ) -> Result<AgentRun, DbError> {
        let id = new_id();
        let now = Utc::now();
        let conn = self.db.conn()?;
        conn.execute(
            "INSERT INTO agent_runs (id, project_id, user_id, agent, model, summary, edits, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![id, project_id, user.id, agent, model, summary, edits, now],
        )?;
        Ok(AgentRun {
            id,
            agent: agent.to_string(),
            model: model.map(str::to_string),
            summary: summary.to_string(),
            edits,
            author_name: user.name.clone(),
            created_at: now,
        })
    }

    pub fn agent_runs(&self, project_id: &str) -> Result<Vec<AgentRun>, DbError> {
        let conn = self.db.conn()?;
        let mut stmt = conn.prepare(
            "SELECT a.id, a.agent, a.model, a.summary, a.edits, u.name, a.created_at
             FROM agent_runs a LEFT JOIN users u ON u.id = a.user_id
             WHERE a.project_id = ?1 ORDER BY a.created_at DESC LIMIT 200",
        )?;
        let rows = stmt
            .query_map(params![project_id], |r| {
                Ok(AgentRun {
                    id: r.get(0)?, agent: r.get(1)?, model: r.get(2)?, summary: r.get(3)?,
                    edits: r.get(4)?, author_name: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                    created_at: r.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // --- audit -----------------------------------------------------------------------------

    pub fn audit(&self, project_id: Option<&str>, actor: Option<&User>, action: &str, detail: Option<&str>) {
        let conn = match self.db.conn() {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = %e, "audit log unavailable");
                return;
            }
        };
        let (actor_id, actor_name) = match actor {
            Some(u) => (Some(u.id.as_str()), Some(u.name.as_str())),
            None => (None, None),
        };
        if let Err(e) = conn.execute(
            "INSERT INTO audit_log (project_id, actor_id, actor_name, action, detail, created_at) VALUES (?1,?2,?3,?4,?5,?6)",
            params![project_id, actor_id, actor_name, action, detail, Utc::now()],
        ) {
            tracing::error!(error = %e, action, "failed to write audit entry");
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{0}")]
    Db(#[from] DbError),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    NotFound(String),
}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError::Db(e.into())
    }
}

fn row_to_user(r: &rusqlite::Row<'_>) -> rusqlite::Result<User> {
    Ok(User {
        id: r.get(0)?,
        email: r.get(1)?,
        name: r.get(2)?,
        pw_hash: r.get(3)?,
        is_admin: r.get::<_, i64>(4)? != 0,
        is_guest: r.get::<_, i64>(5)? != 0,
    })
}

fn row_to_share(r: &rusqlite::Row<'_>) -> rusqlite::Result<ShareLink> {
    Ok(ShareLink {
        id: r.get(0)?,
        project_id: r.get(1)?,
        role: Role::parse(&r.get::<_, String>(2)?).unwrap_or(Role::Viewer),
        label: r.get(3)?,
        expires_at: r.get(4)?,
        created_at: r.get(5)?,
        revoked: r.get::<_, i64>(6)? != 0,
    })
}

fn row_to_comment(r: &rusqlite::Row<'_>) -> rusqlite::Result<Comment> {
    Ok(Comment {
        id: r.get(0)?,
        file: r.get(1)?,
        anchor: r.get(2)?,
        quote: r.get(3)?,
        author_id: r.get(4)?,
        author_name: r.get(5)?,
        body: r.get(6)?,
        resolved: r.get::<_, i64>(7)? != 0,
        created_at: r.get(8)?,
    })
}

fn row_to_request(r: &rusqlite::Row<'_>) -> rusqlite::Result<RoleRequest> {
    let payload_str: String = r.get(3)?;
    Ok(RoleRequest {
        id: r.get(0)?,
        kind: r.get(1)?,
        target_id: r.get(2)?,
        payload: serde_json::from_str(&payload_str).unwrap_or(serde_json::Value::Null),
        summary: r.get(4)?,
        proposer_id: r.get(5)?,
        proposer_name: r.get(6)?,
        status: r.get(7)?,
        created_at: r.get(8)?,
        votes: Vec::new(),
    })
}

fn row_to_suggestion(r: &rusqlite::Row<'_>) -> rusqlite::Result<Suggestion> {
    Ok(Suggestion {
        id: r.get(0)?,
        file: r.get(1)?,
        anchor: r.get(2)?,
        anchor_end: r.get(3)?,
        quote: r.get(4)?,
        replacement: r.get(5)?,
        author_id: r.get(6)?,
        author_name: r.get(7)?,
        status: r.get(8)?,
        created_at: r.get(9)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::new(Db::open_in_memory().unwrap())
    }

    #[test]
    fn users_sessions_and_membership() {
        let s = store();
        assert_eq!(s.user_count().unwrap(), 0);
        let admin = s.create_user("Admin@Uni.edu", "Admin", "supersecret", true).unwrap();
        assert_eq!(admin.email, "admin@uni.edu");
        assert_eq!(s.user_count().unwrap(), 1);
        assert!(matches!(s.create_user("admin@uni.edu", "Dup", "supersecret", false), Err(StoreError::Conflict(_))));
        assert!(matches!(s.create_user("x@y.z", "X", "short", false), Err(StoreError::Invalid(_))));

        let token = s.create_session(&admin.id, 30).unwrap();
        assert_eq!(s.session_user(&token).unwrap().unwrap().id, admin.id);
        s.delete_session(&token).unwrap();
        assert!(s.session_user(&token).unwrap().is_none());

        s.set_member("p1", &admin.id, Role::Admin).unwrap();
        assert_eq!(s.role_of("p1", &admin.id).unwrap(), Some(Role::Admin));
        assert_eq!(s.owned_projects(&admin.id).unwrap(), vec!["p1"]);
        s.set_member("p1", &admin.id, Role::Editor).unwrap(); // upsert
        assert_eq!(s.role_of("p1", &admin.id).unwrap(), Some(Role::Editor));
    }

    #[test]
    fn share_links_resolve_and_expire() {
        let s = store();
        let owner = s.create_user("o@x.y", "Owner", "supersecret", true).unwrap();
        let (link, token) = s.create_share("p1", Role::Commenter, Some("Reviewers"), None, &owner.id).unwrap();
        assert_eq!(s.share_by_token(&token).unwrap(), Some(("p1".into(), Role::Commenter)));
        assert_eq!(s.shares("p1").unwrap().len(), 1);
        assert!(s.revoke_share("p1", &link.id).unwrap());
        assert!(s.share_by_token(&token).unwrap().is_none());
        assert!(s.shares("p1").unwrap().is_empty());

        let past = Utc::now() - chrono::Duration::days(1);
        let (_, expired) = s.create_share("p1", Role::Viewer, None, Some(past), &owner.id).unwrap();
        assert!(s.share_by_token(&expired).unwrap().is_none());
    }

    #[test]
    fn comments_and_suggestions() {
        let s = store();
        let u = s.create_user("a@b.c", "Ana", "supersecret", false).unwrap();
        let c = s.add_comment("p1", "main.tex", "anchor1", Some("the claim"), &u, "needs a citation").unwrap();
        assert_eq!(s.comments("p1").unwrap().len(), 1);
        assert!(s.resolve_comment("p1", &c.id, true).unwrap());
        assert!(s.comments("p1").unwrap()[0].resolved);
        assert_eq!(s.comment_author("p1", &c.id).unwrap().as_deref(), Some(u.id.as_str()));
        assert!(matches!(s.add_comment("p1", "main.tex", "a", None, &u, "  "), Err(StoreError::Invalid(_))));

        let sug = s.add_suggestion("p1", "main.tex", "s0", "s1", "utilizes", "uses", &u).unwrap();
        assert_eq!(s.suggestions("p1").unwrap().len(), 1);
        assert!(s.set_suggestion_status("p1", &sug.id, "accepted").unwrap());
        assert!(!s.set_suggestion_status("p1", &sug.id, "rejected").unwrap()); // already closed
        assert_eq!(s.suggestions("p1").unwrap()[0].status, "accepted");
    }

    #[test]
    fn governance_thresholds() {
        use Decision::*;
        use GovernanceMode::*;
        assert_eq!(Unanimous.evaluate(3, 3, 0), Pass);
        assert_eq!(Unanimous.evaluate(3, 2, 0), Pending);
        assert_eq!(Unanimous.evaluate(3, 2, 1), Fail);
        assert_eq!(Majority.evaluate(3, 2, 0), Pass); // 2 of 3 is a majority
        assert_eq!(Majority.evaluate(3, 1, 0), Pending);
        assert_eq!(Majority.evaluate(4, 2, 2), Fail); // half reject, so a majority is impossible
        assert_eq!(Majority.evaluate(1, 1, 0), Pass); // a sole admin acts alone
        assert_eq!(Solo.evaluate(5, 1, 0), Pass);
        assert_eq!(Unanimous.evaluate(0, 0, 0), Pending); // no admins yet
    }

    #[test]
    fn unanimous_request_waits_then_applies() {
        let s = store();
        let ana = s.create_user("ana@x.y", "Ana", "supersecret", true).unwrap();
        let bob = s.create_user("bob@x.y", "Bob", "supersecret", false).unwrap();
        let carol = s.create_user("carol@x.y", "Carol", "supersecret", false).unwrap();
        s.set_member("p", &ana.id, Role::Admin).unwrap();
        s.set_member("p", &bob.id, Role::Admin).unwrap();
        s.set_governance_now("p", GovernanceMode::Unanimous).unwrap();
        assert_eq!(s.admin_ids("p").unwrap().len(), 2);

        let req = s
            .create_request("p", "invite", Some(&carol.id), &serde_json::json!({ "email": carol.email, "role": "viewer" }), "add carol", &ana)
            .unwrap();
        // Only the proposer (Ana) has approved so far.
        assert_eq!(s.evaluate_request("p", &req.id).unwrap(), Resolution::Pending);
        assert!(s.role_of("p", &carol.id).unwrap().is_none());

        s.record_vote(&req.id, &bob.id, true).unwrap();
        assert_eq!(s.evaluate_request("p", &req.id).unwrap(), Resolution::Applied);
        assert_eq!(s.role_of("p", &carol.id).unwrap(), Some(Role::Viewer));
        assert!(s.open_requests("p").unwrap().is_empty());
    }

    #[test]
    fn one_reject_fails_a_unanimous_request() {
        let s = store();
        let ana = s.create_user("ana@x.y", "Ana", "supersecret", true).unwrap();
        let bob = s.create_user("bob@x.y", "Bob", "supersecret", false).unwrap();
        s.set_member("p", &ana.id, Role::Admin).unwrap();
        s.set_member("p", &bob.id, Role::Admin).unwrap();
        s.set_governance_now("p", GovernanceMode::Unanimous).unwrap();
        let req = s.create_request("p", "set_governance", None, &serde_json::json!({ "governance": "solo" }), "loosen", &ana).unwrap();
        s.record_vote(&req.id, &bob.id, false).unwrap();
        assert_eq!(s.evaluate_request("p", &req.id).unwrap(), Resolution::Rejected);
        assert_eq!(s.governance("p").unwrap(), GovernanceMode::Unanimous); // unchanged
    }

    #[test]
    fn guests_have_no_email_and_dont_count() {
        let s = store();
        let g1 = s.create_guest("Reviewer 1").unwrap();
        let g2 = s.create_guest("Reviewer 1").unwrap();
        assert_ne!(g1.id, g2.id);
        assert!(g1.is_guest && g1.email.is_empty());
        assert_eq!(s.user_count().unwrap(), 0);
    }
}
