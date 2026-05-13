//! Tokio runtime bootstrap for the server binary.

const WORKER_STACK_SIZE: usize = 8 * 1024 * 1024;

pub(crate) fn block_on_server<F>(future: F)
where
    F: std::future::Future<Output = ()>,
{
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(WORKER_STACK_SIZE)
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("failed to build tokio runtime: {error}");
            std::process::exit(1);
        }
    };
    runtime.block_on(future);
}
