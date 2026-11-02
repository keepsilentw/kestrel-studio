use std::collections::HashMap;

use axum::extract::{Path as UrlPath, Query, State};
use axum::extract::Form;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::admin::service::{
    AccountView, AdminService, CreateAccountResult, RemoveAccountResult, ResetPasswordResult,
};
use crate::auth::guards::SuperAdmin;
use crate::common::http::read_positive_int;
use crate::error::AppError;
use crate::http::redirect::found;
use crate::state::AppState;
use crate::view::admin_templates::{
    AccountChip, ConversationRow, Notice, PresentedAccount, account_page, accounts_page,
    conversation_page, conversations_page, delete_page,
};
use crate::view::render::{format_timestamp, render_messages};

/// The super admin's surface: account management and a read-only view of every
/// normal account's conversations. Server-rendered forms with POST + redirect,
/// exactly like /login and /logout — there is no client script on these pages.

fn notice(code: Option<&str>) -> Option<Notice> {
    let text = match code? {
        "created" => Some(("账号已创建", false)),
        "duplicate" => Some(("该用户名已存在", true)),
        "invalid" => Some(("用户名需 3-32 位字母、数字、下划线或短横线；密码至少 6 位", true)),
        "password-reset" => Some(("密码已重置", false)),
        "removed" => Some(("账号及其全部会话与资产已删除", false)),
        "self" => Some(("不能删除当前登录的账号", true)),
        "last-super" => Some(("不能删除最后一个超级管理员", true)),
        "missing" => Some(("账号或会话不存在", true)),
        _ => None,
    };
    text.map(|(text, is_error)| Notice { text, is_error })
}

fn present(account: AccountView) -> PresentedAccount {
    PresentedAccount {
        id: account.id,
        username: account.username,
        role_label: account.role_label.to_string(),
        created_at_label: format_timestamp(account.created_at),
        conversation_count: account.conversation_count,
        asset_count: account.asset_count,
    }
}

fn html(body: String) -> Response {
    (
        axum::http::StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("text/html; charset=utf-8"),
        )],
        body,
    )
        .into_response()
}

/// Passwords are hashed as typed: trimming one would silently change it.
#[derive(Debug, Default, Deserialize)]
pub struct AccountForm {
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
}

fn admin(pool: &AppState) -> AdminService {
    AdminService::new(pool.pool.clone(), pool.conversations.clone())
}

pub async fn index(
    SuperAdmin(actor): SuperAdmin,
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    let accounts = admin(&state).list().await?;
    let note = notice(params.get("notice").map(String::as_str));
    Ok(html(accounts_page(
        &actor.username,
        &accounts.into_iter().map(present).collect::<Vec<_>>(),
        note.as_ref(),
    )))
}

/// Site-wide conversation list; `?user=` narrows it to one account.
pub async fn conversations_page_handler(
    SuperAdmin(actor): SuperAdmin,
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    let service = admin(&state);
    let accounts = service.list().await?;
    let filter = read_positive_int(params.get("user").map(String::as_str));
    let owner = filter.and_then(|id| accounts.iter().find(|item| item.id == id).cloned());

    let note = notice(params.get("notice").map(String::as_str));
    let all = state.conversations.list_all().await?;
    let rows: Vec<ConversationRow> = all
        .iter()
        .filter(|item| owner.as_ref().is_none_or(|owned| item.owner_id == owned.id))
        .map(|item| ConversationRow {
            id: item.id,
            title: item.title.clone(),
            owner_name: item.owner_name.clone(),
            updated_at_label: format_timestamp(item.updated_at),
            deleted_at_label: item.deleted_at.map(format_timestamp),
        })
        .collect();
    let chips: Vec<AccountChip> = accounts
        .iter()
        .map(|item| AccountChip {
            id: item.id,
            username: item.username.clone(),
            is_active: owner.as_ref().is_some_and(|owned| owned.id == item.id),
        })
        .collect();

    Ok(html(conversations_page(
        &actor.username,
        &rows,
        &chips,
        owner.as_ref().map(|item| item.username.as_str()),
        all.len(),
        note.as_ref(),
    )))
}

/// Read-only replay of one conversation, whoever it belongs to.
pub async fn conversation(
    SuperAdmin(actor): SuperAdmin,
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> Result<Response, AppError> {
    let missing = found("/admin/conversations?notice=missing");
    let Some(conversation_id) = read_positive_int(Some(&id)) else {
        return Ok(missing);
    };
    if !state.conversations.can_read(conversation_id, &actor).await? {
        return Ok(missing);
    }
    let Some(summary) = state.conversations.summary_of(conversation_id).await? else {
        return Ok(missing);
    };

    let views = state.conversations.views(conversation_id).await?;
    Ok(html(conversation_page(
        &actor.username,
        &summary.title,
        &summary.owner_name,
        &format_timestamp(summary.updated_at),
        summary.deleted_at.map(format_timestamp).as_deref(),
        summary.owner_id == actor.id,
        &render_messages(views),
    )))
}

/// One account: its facts, the password reset and the way to delete it.
pub async fn account(
    SuperAdmin(actor): SuperAdmin,
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    let missing = found("/admin?notice=missing");
    let Some(account_id) = read_positive_int(Some(&id)) else {
        return Ok(missing);
    };
    let Some(found_account) = admin(&state).find(account_id).await? else {
        return Ok(missing);
    };
    let note = notice(params.get("notice").map(String::as_str));
    Ok(html(account_page(
        &actor.username,
        &present(found_account),
        note.as_ref(),
    )))
}

pub async fn confirm_delete(
    SuperAdmin(actor): SuperAdmin,
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> Result<Response, AppError> {
    let missing = found("/admin?notice=missing");
    let Some(account_id) = read_positive_int(Some(&id)) else {
        return Ok(missing);
    };
    let Some(found_account) = admin(&state).find(account_id).await? else {
        return Ok(missing);
    };
    Ok(html(delete_page(&actor.username, &present(found_account))))
}

pub async fn create_user(
    SuperAdmin(_actor): SuperAdmin,
    State(state): State<AppState>,
    Form(form): Form<AccountForm>,
) -> Result<Response, AppError> {
    let result = admin(&state).create(&form.username, &form.password).await?;
    let code = match result {
        CreateAccountResult::Created(_) => "created",
        // A taken name is its own notice; anything else is "the form was wrong".
        CreateAccountResult::Duplicate => "duplicate",
        CreateAccountResult::InvalidUsername | CreateAccountResult::InvalidPassword => "invalid",
    };
    Ok(found(&format!("/admin?notice={code}")))
}

pub async fn reset_password(
    SuperAdmin(_actor): SuperAdmin,
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
    Form(form): Form<AccountForm>,
) -> Result<Response, AppError> {
    let Some(account_id) = read_positive_int(Some(&id)) else {
        return Ok(found("/admin?notice=missing"));
    };
    let result = admin(&state).reset_password(account_id, &form.password).await?;
    let code = match result {
        ResetPasswordResult::Reset => "password-reset",
        // Back to the account page: a rejected password reads as 'invalid', the
        // same notice the create form shows for it.
        ResetPasswordResult::InvalidPassword => "invalid",
        ResetPasswordResult::Missing => return Ok(found("/admin?notice=missing")),
    };
    Ok(found(&format!("/admin/users/{account_id}?notice={code}")))
}

pub async fn remove_user(
    SuperAdmin(actor): SuperAdmin,
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> Result<Response, AppError> {
    let Some(account_id) = read_positive_int(Some(&id)) else {
        return Ok(found("/admin?notice=missing"));
    };
    let code = match admin(&state).remove(account_id, &actor).await? {
        RemoveAccountResult::Removed { .. } => "removed",
        RemoveAccountResult::Missing => "missing",
        RemoveAccountResult::SelfAccount => "self",
        RemoveAccountResult::LastSuperAdmin => "last-super",
    };
    Ok(found(&format!("/admin?notice={code}")))
}
