#![cfg(feature = "fs-store")]
//! Native local-only pressure regressions. No endpoint, relay or network is used.
use std::{collections::BTreeMap, time::Duration};

use bytes::Bytes;
use iroh_blobs::{store::fs::FsStore, Hash};
use n0_future::{stream, BufferedStreamExt, StreamExt};
use testresult::TestResult;
use tokio::time::timeout;

const IMPORTS: usize = 20_000;
const CONCURRENCY: usize = 64;

fn value(index: usize, distinct: usize, large: bool) -> Bytes {
    if index % 101 == 0 {
        return Bytes::new();
    }
    let mut data = if large {
        vec![0x61; 20 * 1024]
    } else {
        vec![0x61; 32]
    };
    data[..8].copy_from_slice(&((index % distinct) as u64).to_le_bytes());
    Bytes::from(data)
}

async fn ordinary(distinct: usize, large: bool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("store");
    let store = FsStore::load(&path).await?;
    let mut retained = BTreeMap::new();
    let mut imports = stream::iter(0..IMPORTS)
        .map(|index| {
            let store = store.clone();
            async move {
                let data = value(index, distinct, large);
                let expected = Hash::new(&data);
                let tag = store.add_bytes(data.clone()).temp_tag().await?;
                assert_eq!(tag.hash(), expected);
                Ok::<_, anyhow::Error>((tag, data))
            }
        })
        .buffered_unordered(CONCURRENCY);
    while let Some(result) = imports.next().await {
        let (tag, data) = result?;
        retained.entry(tag.hash()).or_insert((tag, data));
    }
    drop(imports);
    for (hash, (tag, data)) in &retained {
        assert_eq!(store.get_bytes(*hash).await?, *data);
        store
            .tags()
            .set(hash.to_string(), tag.hash_and_format())
            .await?;
    }
    store.wait_idle().await?;
    store.sync_db().await?;
    store.shutdown().await?;
    drop(store);
    let reopened = FsStore::load(&path).await?;
    for (hash, (_, data)) in &retained {
        assert_eq!(reopened.get_bytes(*hash).await?, *data);
    }
    reopened.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn twenty_thousand_equal_ordinary_imports_flush_and_reopen() -> TestResult {
    timeout(Duration::from_secs(180), ordinary(1, false)).await?
}

#[tokio::test]
async fn twenty_thousand_mixed_file_backed_imports_flush_and_reopen() -> TestResult {
    timeout(Duration::from_secs(240), ordinary(32, true)).await?
}

#[tokio::test]
async fn twenty_thousand_unique_imports_flush_and_reopen() -> TestResult {
    timeout(Duration::from_secs(240), ordinary(IMPORTS, false)).await?
}

#[tokio::test]
async fn twenty_thousand_equal_batch_imports_and_scope_release() -> TestResult {
    timeout(Duration::from_secs(180), async {
        let directory = tempfile::tempdir()?;
        let store = FsStore::load(directory.path()).await?;
        let batch = store.blobs().batch().await?;
        let data = Bytes::from_static(b"one native batch value");
        let expected = Hash::new(&data);
        let mut imports = stream::iter(0..IMPORTS)
            .map(|_| {
                let batch = &batch;
                let data = data.clone();
                async move { batch.add_bytes(data).await }
            })
            .buffered_unordered(CONCURRENCY);
        let mut retained = None;
        while let Some(tag) = imports.next().await {
            let tag = tag?;
            assert_eq!(tag.hash(), expected);
            retained = Some(tag);
        }
        drop(imports);
        assert_eq!(retained.as_ref().unwrap().hash(), expected);
        assert_eq!(store.get_bytes(expected).await?, data);
        let tags = store
            .tags()
            .list_temp_tags()
            .await?
            .collect::<Vec<_>>()
            .await;
        assert!(tags.iter().any(|value| value.hash == expected));
        drop(batch);
        store.wait_idle().await?;
        store.sync_db().await?;
        let tags = store
            .tags()
            .list_temp_tags()
            .await?
            .collect::<Vec<_>>()
            .await;
        assert!(!tags.iter().any(|value| value.hash == expected));
        drop(retained);
        store.shutdown().await?;
        Ok(())
    })
    .await?
}
