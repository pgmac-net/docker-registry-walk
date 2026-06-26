use serde::Deserialize;

#[derive(Deserialize)]
struct SearchResponse {
    results: Vec<SearchResult>,
}

#[derive(Deserialize)]
struct SearchResult {
    repo_name: String,
}

pub async fn search_dockerhub(query: &str) -> anyhow::Result<Vec<String>> {
    if query.trim().is_empty() {
        return Ok(vec![]);
    }
    let mut url = url::Url::parse("https://hub.docker.com/v2/search/repositories/")?;
    url.query_pairs_mut()
        .append_pair("query", query)
        .append_pair("page_size", "25");
    let resp = reqwest::Client::new()
        .get(url)
        .send()
        .await?
        .error_for_status()?;
    let body: SearchResponse = resp.json().await?;
    Ok(body.results.into_iter().map(|r| r.repo_name).collect())
}

pub fn is_dockerhub_url(url: &str) -> bool {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .map(|h| {
            matches!(
                h.as_str(),
                "registry-1.docker.io" | "docker.io" | "index.docker.io"
            )
        })
        .unwrap_or(false)
}
