/// A replayed conversation turn, in the shape the Responses API takes as input.
///
/// Tool round-trips are not replayed: their outcome is already reflected in the
/// assistant text, and replaying call ids across turns would require keeping the
/// full provider-side item log.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct InputItem {
    pub role: String,
    pub content: String,
}

impl InputItem {
    #[cfg(test)]
    pub fn user(content: &str) -> Self {
        Self { role: "user".to_string(), content: content.to_string() }
    }
}

pub mod responses;
pub mod token;

