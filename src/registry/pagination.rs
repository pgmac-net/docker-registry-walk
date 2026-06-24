/// Parse `Link: <url>; rel="next"` header and return the next URL if present.
pub fn parse_next_link(link_header: &str) -> Option<String> {
    for part in link_header.split(',') {
        let part = part.trim();
        let mut url: Option<&str> = None;
        let mut is_next = false;

        for segment in part.split(';') {
            let segment = segment.trim();
            if segment.starts_with('<') && segment.ends_with('>') {
                url = Some(&segment[1..segment.len() - 1]);
            } else if segment.eq_ignore_ascii_case(r#"rel="next""#) {
                is_next = true;
            }
        }

        if is_next {
            return url.map(str::to_owned);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_next_link() {
        let h = r#"</v2/_catalog?last=foo&n=100>; rel="next""#;
        assert_eq!(
            parse_next_link(h),
            Some("/v2/_catalog?last=foo&n=100".to_owned())
        );
    }

    #[test]
    fn ignores_non_next_rel() {
        let h = r#"</v2/_catalog>; rel="prev""#;
        assert_eq!(parse_next_link(h), None);
    }

    #[test]
    fn handles_multiple_links() {
        let h = r#"</v2/_catalog?last=a&n=10>; rel="prev", </v2/_catalog?last=b&n=10>; rel="next""#;
        assert_eq!(
            parse_next_link(h),
            Some("/v2/_catalog?last=b&n=10".to_owned())
        );
    }

    #[test]
    fn returns_none_for_empty_header() {
        assert_eq!(parse_next_link(""), None);
    }
}
