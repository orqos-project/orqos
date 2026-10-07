use axum::http::StatusCode;

/// Query strings carry one JSON array, URL-encoded by the client.
pub(crate) fn decode_command(raw: &str) -> Result<Vec<String>, (StatusCode, String)> {
    let command: Vec<String> = serde_json::from_str(raw).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            "cmd must be a JSON array of strings".into(),
        )
    })?;
    if command.is_empty() || command[0].is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Command cannot be empty".into()));
    }
    Ok(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_arguments_without_shell_interpretation() {
        assert_eq!(
            decode_command(r#"["printf","a b\n","","$HOME"]"#).unwrap(),
            ["printf", "a b\n", "", "$HOME"]
        );
    }

    #[test]
    fn rejects_invalid_or_empty_commands() {
        for command in ["echo", "[]", r#"[""]"#, r#"["echo",7]"#, "null", "{}"] {
            assert_eq!(
                decode_command(command).unwrap_err().0,
                StatusCode::BAD_REQUEST
            );
        }
    }
}
