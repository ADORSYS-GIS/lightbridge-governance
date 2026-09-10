use super::join_spool;

#[tokio::test]
async fn cancelled_spool_work_is_an_error_without_a_panic() {
    let task = tokio::spawn(std::future::pending::<anyhow::Result<()>>());
    task.abort();
    let result = join_spool(task.await);
    assert!(result.is_err());
}

#[tokio::test]
async fn genuine_spool_panics_still_propagate() {
    let task = tokio::spawn(async {
        panic!("spool panic sentinel");
    });
    let joined: Result<anyhow::Result<()>, _> = task.await;
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| join_spool(joined)));
    let payload = panic.expect_err("the original panic must propagate");
    assert_eq!(
        payload.downcast_ref::<&str>(),
        Some(&"spool panic sentinel")
    );
}
