//! HTTP request metadata parsing.




pub(crate) fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|s| !s.is_empty())
        .map(|kv| match kv.split_once('=') {
            Some((k, v)) => (k.to_string(), v.to_string()),
            None => (kv.to_string(), String::new()),
        })
        .collect()
}


#[cfg(test)]
mod tests {

    use super::*;




    #[test]
    fn query_parsing() {
        assert_eq!(
            parse_query("a=1&b=&c"),
            vec![
                ("a".into(), "1".into()),
                ("b".into(), "".into()),
                ("c".into(), "".into())
            ],
            "empty and missing values parse as empty strings"
        );
    }
}
