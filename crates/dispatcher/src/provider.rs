use chuggernaut_git_provider::GitProvider;

/// Create a git provider based on the URL.
///
/// URLs containing "github.com" use the GitHub API client;
/// all other URLs are treated as Forgejo/Gitea instances.
pub fn create_provider(url: &str, token: &str) -> Box<dyn GitProvider> {
    if url.contains("github.com") {
        Box::new(chuggernaut_github_api::GitHubClient::new(url, token))
    } else {
        Box::new(chuggernaut_forgejo_api::ForgejoClient::new(url, token))
    }
}
