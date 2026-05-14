//! Range-scan paging.
//!
//! Cross-range scans return a continuation token so the client can
//! resume from the next key after each page. Tokens are encoded as
//! `(last_key, page_seq)` and contain no server-side state, so
//! pages can be served by any replica.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ScanPageRequest {
    pub start_key: Vec<u8>,
    pub end_key: Vec<u8>,
    pub limit: u32,
    /// `None` for first page, `Some(token)` for subsequent pages.
    pub continuation: Option<ContinuationToken>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContinuationToken {
    pub last_key: Vec<u8>,
    pub page_seq: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScanPageResponse {
    pub rows: Vec<(Vec<u8>, Vec<u8>)>,
    pub next: Option<ContinuationToken>,
}

pub fn paginate(
    start_key: &[u8],
    end_key: &[u8],
    storage: &[(Vec<u8>, Vec<u8>)],
    limit: u32,
    continuation: Option<&ContinuationToken>,
) -> ScanPageResponse {
    let lower = continuation
        .map(|t| t.last_key.clone())
        .unwrap_or_else(|| start_key.to_vec());
    let upper = end_key.to_vec();
    let iter = storage
        .iter()
        .filter(|(k, _)| {
            k.as_slice() > lower.as_slice() && (upper.is_empty() || k.as_slice() < upper.as_slice())
        })
        .cloned();
    let mut rows = Vec::with_capacity(limit as usize);
    for entry in iter {
        rows.push(entry);
        if rows.len() as u32 >= limit {
            break;
        }
    }
    let next = rows.last().map(|(k, _)| ContinuationToken {
        last_key: k.clone(),
        page_seq: continuation.map(|t| t.page_seq + 1).unwrap_or(1),
    });
    // Only return a token when more rows exist after our last entry.
    let next = next.filter(|tok| {
        storage.iter().any(|(k, _)| {
            k.as_slice() > tok.last_key.as_slice()
                && (upper.is_empty() || k.as_slice() < upper.as_slice())
        })
    });
    ScanPageResponse { rows, next }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<(Vec<u8>, Vec<u8>)> {
        (0..20u8).map(|i| (vec![i], vec![i * 2])).collect()
    }

    #[test]
    fn first_page_starts_at_start_key() {
        let store = fixture();
        let resp = paginate(&[], &[], &store, 5, None);
        assert_eq!(resp.rows.len(), 5);
        let keys: Vec<u8> = resp.rows.iter().map(|(k, _)| k[0]).collect();
        assert_eq!(keys, vec![0, 1, 2, 3, 4]);
        assert!(resp.next.is_some());
    }

    #[test]
    fn next_page_resumes_from_continuation() {
        let store = fixture();
        let page1 = paginate(&[], &[], &store, 5, None);
        let page2 = paginate(&[], &[], &store, 5, page1.next.as_ref());
        let keys: Vec<u8> = page2.rows.iter().map(|(k, _)| k[0]).collect();
        assert_eq!(keys, vec![5, 6, 7, 8, 9]);
    }

    #[test]
    fn last_page_has_no_next_token() {
        let store = fixture();
        let resp = paginate(&[], &[], &store, 30, None);
        assert_eq!(resp.rows.len(), 20);
        assert!(resp.next.is_none());
    }

    #[test]
    fn end_key_caps_results() {
        let store = fixture();
        let resp = paginate(&[], &[10], &store, 100, None);
        assert!(resp.rows.iter().all(|(k, _)| k[0] < 10));
    }

    #[test]
    fn empty_range_returns_no_rows() {
        let store = fixture();
        let resp = paginate(&[100], &[200], &store, 10, None);
        assert!(resp.rows.is_empty());
        assert!(resp.next.is_none());
    }
}
