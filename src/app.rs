use std::{fs::create_dir_all, io, path::PathBuf, sync::Arc};

use actix::{self, Actor, Addr};

use actix_web::{server, App};

use failure::Fail;

use structopt::StructOpt;

use tokio_threadpool::ThreadPool;

use crate::middlewares::Metrics;

use crate::{
    config::{get_config, Config, ConfigError},
    metrics,
};

use crate::{
    actors::{
        cache::CacheActor, objects::ObjectsActor, symbolication::SymbolicationActor,
        symcaches::SymCacheActor,
    },
    endpoints,
};

#[derive(Fail, Debug, derive_more::From)]
pub enum CliError {
    #[fail(display = "Failed loading config: {}", _0)]
    ConfigParsing(#[fail(cause)] ConfigError),

    #[fail(display = "Failed loading cache dirs: {}", _0)]
    CacheIo(#[fail(cause)] io::Error),
}

#[derive(StructOpt)]
struct Cli {
    /// Path to your configuration file. Defaults to `./config`.
    #[structopt(
        long = "config",
        short = "c",
        raw(global = "true"),
        value_name = "FILE"
    )]
    pub config: Option<PathBuf>,

    #[structopt(subcommand)]
    command: Command,
}

#[derive(StructOpt)]
#[structopt(bin_name = "symbolicator")]
enum Command {
    /// Run server
    #[structopt(name = "run")]
    Run,
}

#[derive(Clone)]
pub struct ServiceState {
    pub symbolication: Addr<SymbolicationActor>,
}

pub type ServiceApp = App<ServiceState>;

pub fn run_main() -> Result<(), CliError> {
    env_logger::init();
    let cli = Cli::from_args();
    let config = get_config(cli.config)?;

    match cli.command {
        Command::Run => run_server(config)?,
    }

    Ok(())
}

pub fn run_server(config: Config) -> Result<(), CliError> {
    if let Some(ref metrics) = config.metrics {
        metrics::configure_statsd(&metrics.prefix, &metrics.statsd);
    }

    let sys = actix::System::new("symbolicator");

    let cpu_threadpool = Arc::new(ThreadPool::new());
    let io_threadpool = Arc::new(ThreadPool::new());

    let download_cache_path = config.cache_dir.as_ref().map(|x| x.join("./objects/"));
    if let Some(ref download_cache_path) = download_cache_path {
        create_dir_all(download_cache_path)?;
    }
    let download_cache = CacheActor::new(download_cache_path).start();
    let objects = ObjectsActor::new(download_cache, io_threadpool.clone()).start();

    let symcache_path = config.cache_dir.as_ref().map(|x| x.join("./symcaches/"));
    if let Some(ref symcache_path) = symcache_path {
        create_dir_all(symcache_path)?;
    }
    let symcache_cache = CacheActor::new(symcache_path).start();
    let symcaches = SymCacheActor::new(symcache_cache, objects, cpu_threadpool.clone()).start();

    let symbolication = SymbolicationActor::new(symcaches, cpu_threadpool.clone()).start();

    let state = ServiceState { symbolication };

    fn get_app(state: ServiceState) -> ServiceApp {
        let mut app = App::with_state(state).middleware(Metrics);
        app = endpoints::symbolicate::register(app);
        app = endpoints::healthcheck::register(app);
        app
    }

    server::new(move || get_app(state.clone()))
        .bind(&config.bind)
        .unwrap()
        .start();

    println!("Started http server: {}", config.bind);
    let _ = sys.run();
    Ok(())
}
