// Copyright 2015, 2016 Ethcore (UK) Ltd.
// This file is part of Parity.

// Parity is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Parity is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Parity.  If not, see <http://www.gnu.org/licenses/>.

use std::sync::{Arc, Mutex, Condvar};
use std::net::{TcpListener};
use ctrlc::CtrlC;
use fdlimit::raise_fd_limit;
use ethcore_rpc::{NetworkSettings, is_major_importing};
use ethsync::NetworkConfiguration;
use util::{Colour, version, RotatingLogger};
use io::{MayPanic, ForwardPanic, PanicHandler};
use ethcore_logger::{Config as LogConfig};
use ethcore::client::{Mode, DatabaseCompactionProfile, VMType, ChainNotify, BlockChainClient};
use ethcore::service::ClientService;
use ethcore::account_provider::AccountProvider;
use ethcore::miner::{Miner, MinerService, ExternalMiner, MinerOptions};
use ethcore::snapshot;
use ethsync::SyncConfig;
use informant::Informant;

use rpc::{HttpServer, IpcServer, HttpConfiguration, IpcConfiguration};
use signer::SignerServer;
use dapps::WebappServer;
use io_handler::ClientIoHandler;
use params::{
	SpecType, Pruning, AccountsConfig, GasPricerConfig, MinerExtras, Switch,
	tracing_switch_to_bool, fatdb_switch_to_bool, mode_switch_to_bool
};
use helpers::{to_client_config, execute_upgrades, passwords_from_files};
use dir::Directories;
use cache::CacheConfig;
use user_defaults::UserDefaults;
use dapps;
use signer;
use modules;
use rpc_apis;
use rpc;
use url;

// how often to take periodic snapshots.
const SNAPSHOT_PERIOD: u64 = 10000;

// how many blocks to wait before starting a periodic snapshot.
const SNAPSHOT_HISTORY: u64 = 100;

#[derive(Debug, PartialEq)]
pub struct RunCmd {
	pub cache_config: CacheConfig,
	pub dirs: Directories,
	pub spec: SpecType,
	pub pruning: Pruning,
	pub pruning_history: u64,
	/// Some if execution should be daemonized. Contains pid_file path.
	pub daemon: Option<String>,
	pub logger_config: LogConfig,
	pub miner_options: MinerOptions,
	pub http_conf: HttpConfiguration,
	pub ipc_conf: IpcConfiguration,
	pub net_conf: NetworkConfiguration,
	pub network_id: Option<usize>,
	pub warp_sync: bool,
	pub acc_conf: AccountsConfig,
	pub gas_pricer: GasPricerConfig,
	pub miner_extras: MinerExtras,
	pub mode: Option<Mode>,
	pub tracing: Switch,
	pub fat_db: Switch,
	pub compaction: DatabaseCompactionProfile,
	pub wal: bool,
	pub vm_type: VMType,
	pub geth_compatibility: bool,
	pub ui_address: Option<(String, u16)>,
	pub net_settings: NetworkSettings,
	pub dapps_conf: dapps::Configuration,
	pub signer_conf: signer::Configuration,
	pub ui: bool,
	pub name: String,
	pub custom_bootnodes: bool,
	pub no_periodic_snapshot: bool,
	pub check_seal: bool,
}

pub fn execute(cmd: RunCmd, logger: Arc<RotatingLogger>) -> Result<(), String> {
	if cmd.ui && cmd.dapps_conf.enabled {
		// Check if Parity is already running
		let addr = format!("{}:{}", cmd.dapps_conf.interface, cmd.dapps_conf.port);
		if !TcpListener::bind(&addr as &str).is_ok() {
			url::open(&format!("http://{}:{}/", cmd.dapps_conf.interface, cmd.dapps_conf.port));
			return Ok(());
		}
	}

	// set up panic handler
	let panic_handler = PanicHandler::new_in_arc();

	// increase max number of open files
	raise_fd_limit();

	// create dirs used by parity
	try!(cmd.dirs.create_dirs(cmd.dapps_conf.enabled, cmd.signer_conf.enabled));

	// load spec
	let spec = try!(cmd.spec.spec());

	// load genesis hash
	let genesis_hash = spec.genesis_header().hash();

	// database paths
	let db_dirs = cmd.dirs.database(genesis_hash, spec.fork_name.clone());

	// user defaults path
	let user_defaults_path = db_dirs.user_defaults_path();

	// load user defaults
	let mut user_defaults = try!(UserDefaults::load(&user_defaults_path));

	// select pruning algorithm
	let algorithm = cmd.pruning.to_algorithm(&user_defaults);

	// check if tracing is on
	let tracing = try!(tracing_switch_to_bool(cmd.tracing, &user_defaults));

	// check if fatdb is on
	let fat_db = try!(fatdb_switch_to_bool(cmd.fat_db, &user_defaults, algorithm));

	// get the mode
	let mode = try!(mode_switch_to_bool(cmd.mode, &user_defaults));
	trace!(target: "mode", "mode is {:?}", mode);
	let network_enabled = match &mode { &Mode::Dark(_) | &Mode::Off => false, _ => true, };

	// prepare client and snapshot paths.
	let client_path = db_dirs.client_path(algorithm);
	let snapshot_path = db_dirs.snapshot_path();

	// execute upgrades
	try!(execute_upgrades(&db_dirs, algorithm, cmd.compaction.compaction_profile(db_dirs.fork_path().as_path())));

	// run in daemon mode
	if let Some(pid_file) = cmd.daemon {
		try!(daemonize(pid_file));
	}

	// display info about used pruning algorithm
	info!("Starting {}", Colour::White.bold().paint(version()));
	info!("State DB configuation: {}{}{}",
		Colour::White.bold().paint(algorithm.as_str()),
		match fat_db {
			true => Colour::White.bold().paint(" +Fat").to_string(),
			false => "".to_owned(),
		},
		match tracing {
			true => Colour::White.bold().paint(" +Trace").to_string(),
			false => "".to_owned(),
		}
	);
	info!("Operating mode: {}", Colour::White.bold().paint(format!("{}", mode)));

	// display warning about using experimental journaldb alorithm
	if !algorithm.is_stable() {
		warn!("Your chosen strategy is {}! You can re-run with --pruning to change.", Colour::Red.bold().paint("unstable"));
	}

	// create sync config
	let mut sync_config = SyncConfig::default();
	sync_config.network_id = match cmd.network_id {
		Some(id) => id,
		None => spec.network_id(),
	};
	if spec.subprotocol_name().len() != 3 {
		warn!("Your chain specification's subprotocol length is not 3. Ignoring.");
	} else {
		sync_config.subprotocol_name.clone_from_slice(spec.subprotocol_name().as_bytes());
	}
	sync_config.fork_block = spec.fork_block();
	sync_config.warp_sync = cmd.warp_sync;

	// prepare account provider
	let account_provider = Arc::new(try!(prepare_account_provider(&cmd.dirs, cmd.acc_conf)));

	// create miner
	let miner = Miner::new(cmd.miner_options, cmd.gas_pricer.into(), &spec, Some(account_provider.clone()));
	miner.set_author(cmd.miner_extras.author);
	miner.set_gas_floor_target(cmd.miner_extras.gas_floor_target);
	miner.set_gas_ceil_target(cmd.miner_extras.gas_ceil_target);
	miner.set_extra_data(cmd.miner_extras.extra_data);
	miner.set_transactions_limit(cmd.miner_extras.transactions_limit);

	// create client config
	let client_config = to_client_config(
		&cmd.cache_config,
		mode,
		tracing,
		fat_db,
		cmd.compaction,
		cmd.wal,
		cmd.vm_type,
		cmd.name,
		algorithm,
		cmd.pruning_history,
		cmd.check_seal,
	);

	// set up bootnodes
	let mut net_conf = cmd.net_conf;
	if !cmd.custom_bootnodes {
		net_conf.boot_nodes = spec.nodes.clone();
	}

	// set network path.
	net_conf.net_config_path = Some(db_dirs.network_path().to_string_lossy().into_owned());

	// create supervisor
	let mut hypervisor = modules::hypervisor(&cmd.dirs.ipc_path());

	// create client service.
	let service = try!(ClientService::start(
		client_config,
		&spec,
		&client_path,
		&snapshot_path,
		&cmd.dirs.ipc_path(),
		miner.clone(),
	).map_err(|e| format!("Client service error: {:?}", e)));

	// forward panics from service
	panic_handler.forward_from(&service);

	// take handle to client
	let client = service.client();
	let snapshot_service = service.snapshot_service();

	// create external miner
	let external_miner = Arc::new(ExternalMiner::default());

	// create sync object
	let (sync_provider, manage_network, chain_notify) = try!(modules::sync(
		&mut hypervisor, sync_config, net_conf.into(), client.clone(), snapshot_service.clone(), &cmd.logger_config,
	).map_err(|e| format!("Sync error: {}", e)));

	service.add_notify(chain_notify.clone());

	// start network
	if network_enabled {
		chain_notify.start();
	}

	// set up dependencies for rpc servers
	let signer_path = cmd.signer_conf.signer_path.clone();
	let deps_for_rpc_apis = Arc::new(rpc_apis::Dependencies {
		signer_service: Arc::new(rpc_apis::SignerService::new(move || {
			signer::generate_new_token(signer_path.clone()).map_err(|e| format!("{:?}", e))
		}, cmd.ui_address)),
		snapshot: snapshot_service.clone(),
		client: client.clone(),
		sync: sync_provider.clone(),
		net: manage_network.clone(),
		secret_store: account_provider.clone(),
		miner: miner.clone(),
		external_miner: external_miner.clone(),
		logger: logger.clone(),
		settings: Arc::new(cmd.net_settings.clone()),
		net_service: manage_network.clone(),
		geth_compatibility: cmd.geth_compatibility,
		dapps_interface: match cmd.dapps_conf.enabled {
			true => Some(cmd.dapps_conf.interface.clone()),
			false => None,
		},
		dapps_port: match cmd.dapps_conf.enabled {
			true => Some(cmd.dapps_conf.port),
			false => None,
		},
	});

	let dependencies = rpc::Dependencies {
		panic_handler: panic_handler.clone(),
		apis: deps_for_rpc_apis.clone(),
	};

	// start rpc servers
	let http_server = try!(rpc::new_http(cmd.http_conf, &dependencies));
	let ipc_server = try!(rpc::new_ipc(cmd.ipc_conf, &dependencies));

	let dapps_deps = dapps::Dependencies {
		panic_handler: panic_handler.clone(),
		apis: deps_for_rpc_apis.clone(),
		client: client.clone(),
		sync: sync_provider.clone(),
	};

	// start dapps server
	let dapps_server = try!(dapps::new(cmd.dapps_conf.clone(), dapps_deps));

	let signer_deps = signer::Dependencies {
		panic_handler: panic_handler.clone(),
		apis: deps_for_rpc_apis.clone(),
	};

	// start signer server
	let signer_server = try!(signer::start(cmd.signer_conf, signer_deps));

	let informant = Arc::new(Informant::new(
		service.client(),
		Some(sync_provider.clone()),
		Some(manage_network.clone()),
		Some(snapshot_service.clone()),
		cmd.logger_config.color
	));
	let info_notify: Arc<ChainNotify> = informant.clone();
	service.add_notify(info_notify);
	let io_handler = Arc::new(ClientIoHandler {
		client: service.client(),
		info: informant,
		sync: sync_provider.clone(),
		net: manage_network.clone(),
		accounts: account_provider.clone(),
		shutdown: Default::default(),
	});
	service.register_io_handler(io_handler.clone()).expect("Error registering IO handler");

	// save user defaults
	user_defaults.pruning = algorithm;
	user_defaults.tracing = tracing;
	try!(user_defaults.save(&user_defaults_path));

	let on_mode_change = move |mode: &Mode| {
		user_defaults.mode = mode.clone();
		let _ = user_defaults.save(&user_defaults_path);	// discard failures - there's nothing we can do
	};

	// tell client how to save the default mode if it gets changed.
	client.on_mode_change(on_mode_change);

	// the watcher must be kept alive.
	let _watcher = match cmd.no_periodic_snapshot {
		true => None,
		false => {
			let sync = sync_provider.clone();
			let watcher = Arc::new(snapshot::Watcher::new(
				service.client(),
				move || is_major_importing(Some(sync.status().state), client.queue_info()),
				service.io().channel(),
				SNAPSHOT_PERIOD,
				SNAPSHOT_HISTORY,
			));

			service.add_notify(watcher.clone());
			Some(watcher)
		},
	};

	// start ui
	if cmd.ui {
		if !cmd.dapps_conf.enabled {
			return Err("Cannot use UI command with Dapps turned off.".into())
		}
		url::open(&format!("http://{}:{}/", cmd.dapps_conf.interface, cmd.dapps_conf.port));
	}

	// Handle exit
	wait_for_exit(panic_handler, http_server, ipc_server, dapps_server, signer_server);

	// to make sure timer does not spawn requests while shutdown is in progress
	io_handler.shutdown.store(true, ::std::sync::atomic::Ordering::SeqCst);
	// just Arc is dropping here, to allow other reference release in its default time
	drop(io_handler);

	// hypervisor should be shutdown first while everything still works and can be
	// terminated gracefully
	drop(hypervisor);

	Ok(())
}

#[cfg(not(windows))]
fn daemonize(pid_file: String) -> Result<(), String> {
	extern crate daemonize;

	daemonize::Daemonize::new()
			.pid_file(pid_file)
			.chown_pid_file(true)
			.start()
			.map(|_| ())
			.map_err(|e| format!("Couldn't daemonize; {}", e))
}

#[cfg(windows)]
fn daemonize(_pid_file: String) -> Result<(), String> {
	Err("daemon is no supported on windows".into())
}

fn prepare_account_provider(dirs: &Directories, cfg: AccountsConfig) -> Result<AccountProvider, String> {
	use ethcore::ethstore::EthStore;
	use ethcore::ethstore::dir::DiskDirectory;

	let passwords = try!(passwords_from_files(cfg.password_files));

	let dir = Box::new(try!(DiskDirectory::create(dirs.keys.clone()).map_err(|e| format!("Could not open keys directory: {}", e))));
	let account_service = AccountProvider::new(Box::new(
		try!(EthStore::open_with_iterations(dir, cfg.iterations).map_err(|e| format!("Could not open keys directory: {}", e)))
	));

	for a in cfg.unlocked_accounts {
		if passwords.iter().find(|p| account_service.unlock_account_permanently(a, (*p).clone()).is_ok()).is_none() {
			return Err(format!("No password found to unlock account {}. Make sure valid password is present in files passed using `--password`.", a));
		}
	}

	Ok(account_service)
}

fn wait_for_exit(
	panic_handler: Arc<PanicHandler>,
	_http_server: Option<HttpServer>,
	_ipc_server: Option<IpcServer>,
	_dapps_server: Option<WebappServer>,
	_signer_server: Option<SignerServer>
	) {
	let exit = Arc::new(Condvar::new());

	// Handle possible exits
	let e = exit.clone();
	CtrlC::set_handler(move || { e.notify_all(); });

	// Handle panics
	let e = exit.clone();
	panic_handler.on_panic(move |_reason| { e.notify_all(); });

	// Wait for signal
	let mutex = Mutex::new(());
	let _ = exit.wait(mutex.lock().unwrap());
	info!("Finishing work, please wait...");
}
