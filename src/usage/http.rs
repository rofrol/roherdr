use std::io::Write;
use std::process::Stdio;

const REQUEST_TIMEOUT_SECS: &str = "15";

pub(super) struct HttpResponse {
    pub(super) status: u16,
    pub(super) body: String,
}

/// GET `url` through curl. Headers travel over stdin so secrets never appear in argv.
pub(super) fn get(url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse, String> {
    let mut child = crate::noninteractive_process::curl_command()
        .args([
            "--silent",
            "--show-error",
            "--max-time",
            REQUEST_TIMEOUT_SECS,
            "--header",
            "@-",
            "--write-out",
            "\n%{http_code}",
            url,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("curl unavailable: {error}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let header_block = headers
            .iter()
            .map(|(name, value)| format!("{name}: {value}\n"))
            .collect::<String>();
        stdin
            .write_all(header_block.as_bytes())
            .map_err(|error| format!("curl stdin failed: {error}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("curl failed: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("request failed: {}", stderr.trim()));
    }
    parse_output(&String::from_utf8_lossy(&output.stdout))
}

fn parse_output(stdout: &str) -> Result<HttpResponse, String> {
    let (body, status) = stdout
        .rsplit_once('\n')
        .ok_or_else(|| "request returned no status".to_owned())?;
    let status = status
        .trim()
        .parse::<u16>()
        .map_err(|_| "request returned an invalid status".to_owned())?;
    Ok(HttpResponse {
        status,
        body: body.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_out_status_is_split_from_body() {
        let response = parse_output("{\"a\":1}\n200").unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, "{\"a\":1}");
    }

    #[test]
    fn empty_body_keeps_status() {
        let response = parse_output("\n401").unwrap();
        assert_eq!(response.status, 401);
        assert!(response.body.is_empty());
    }
}
