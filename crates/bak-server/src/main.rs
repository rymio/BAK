use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use bak_server::{auth, guard, tls, Config};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "bak-server",
    version,
    about = "Backup server for bak (speaks restic's REST protocol)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the server.
    Serve {
        /// Directory holding the repositories.
        #[arg(long, value_name = "DIR")]
        path: PathBuf,
        /// Address and port to listen on.
        #[arg(long, default_value = "0.0.0.0:8000")]
        listen: String,
        /// Refuse deletes and overwrites, so clients can add backups but never remove them.
        #[arg(long)]
        append_only: bool,
        /// Each user may only use repositories under /<username>/.
        #[arg(long)]
        private_repos: bool,
        /// Disable authentication (only for a trusted network).
        #[arg(long, conflicts_with = "htpasswd")]
        no_auth: bool,
        /// Users file (default: <path>/.htpasswd).
        #[arg(long, value_name = "FILE")]
        htpasswd: Option<PathBuf>,
        /// Serve HTTPS. Without --tls-cert/--tls-key, a private CA and server
        /// certificate are generated in <path>/.tls.
        #[arg(long)]
        tls: bool,
        #[arg(long, value_name = "FILE", requires_all = ["tls", "tls_key"])]
        tls_cert: Option<PathBuf>,
        #[arg(long, value_name = "FILE", requires_all = ["tls", "tls_cert"])]
        tls_key: Option<PathBuf>,
        /// Host name or IP clients use to reach the server (repeatable; for
        /// generated certificates). Defaults to this machine's host name,
        /// localhost and 127.0.0.1.
        #[arg(long = "hostname", value_name = "NAME")]
        hostnames: Vec<String>,
        /// Do not serve the web dashboard at /_/.
        #[arg(long)]
        no_dashboard: bool,
        /// Allow plain HTTP on a non-loopback address (only behind a TLS proxy or VPN).
        #[arg(long)]
        allow_http: bool,
        /// Log every request.
        #[arg(short, long)]
        verbose: bool,
    },
    /// Manage users in the htpasswd file.
    #[command(subcommand)]
    User(UserCommand),
}

#[derive(Debug, Subcommand)]
enum UserCommand {
    /// Add a user, or change their password. Reads the password from a prompt,
    /// or from the first line of stdin when it is not a terminal.
    Add {
        name: String,
        /// Generate a strong password and print it, instead of asking for one.
        #[arg(long)]
        generate: bool,
        /// Make the user an administrator (manages users in the dashboard).
        #[arg(long)]
        admin: bool,
        #[command(flatten)]
        file: UsersFile,
    },
    /// Remove a user. Their repositories are not touched.
    Remove {
        name: String,
        #[command(flatten)]
        file: UsersFile,
    },
    /// List users.
    List {
        #[command(flatten)]
        file: UsersFile,
    },
}

#[derive(Debug, clap::Args)]
#[group(required = true, multiple = false)]
struct UsersFile {
    /// Server data directory; uses <path>/.htpasswd.
    #[arg(long, value_name = "DIR")]
    path: Option<PathBuf>,
    /// Users file.
    #[arg(long, value_name = "FILE")]
    htpasswd: Option<PathBuf>,
}

impl UsersFile {
    fn resolve(&self) -> PathBuf {
        match (&self.htpasswd, &self.path) {
            (Some(f), _) => f.clone(),
            (None, Some(p)) => p.join(".htpasswd"),
            (None, None) => PathBuf::from(".htpasswd"),
        }
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("bak-server: error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), String> {
    match cli.command {
        Command::Serve {
            path,
            listen,
            append_only,
            private_repos,
            no_auth,
            htpasswd,
            tls,
            tls_cert,
            tls_key,
            hostnames,
            no_dashboard,
            allow_http,
            verbose,
        } => {
            std::fs::create_dir_all(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let path = path.canonicalize().map_err(|e| e.to_string())?;
            if !tls && !allow_http && !is_loopback(&listen) {
                return Err(format!(
                    "refusing plain HTTP on {listen}: passwords would cross the network unencrypted. \
                     Use --tls, listen on 127.0.0.1, or pass --allow-http behind a TLS proxy or VPN"
                ));
            }
            // Bind before anything is created, so a busy port fails cleanly.
            let std_listener = std::net::TcpListener::bind(&listen)
                .map_err(|e| format!("cannot listen on {listen}: {e}"))?;
            std_listener
                .set_nonblocking(true)
                .map_err(|e| e.to_string())?;
            let tls_config = if tls {
                let (cert, key) = match (tls_cert, tls_key) {
                    (Some(c), Some(k)) => (c, k),
                    _ => {
                        let hosts = if hostnames.is_empty() {
                            default_hostnames()
                        } else {
                            hostnames
                        };
                        let g = tls::ensure_generated(&path.join(".tls"), &hosts)
                            .map_err(|e| format!("generating TLS certificate: {e}"))?;
                        if g.renewed {
                            eprintln!("generated a server certificate for: {}", hosts.join(", "));
                        }
                        eprintln!(
                            "clients must trust {}: copy it to the client and use\n  \
                             bak remote add <name> rest:https://<host>:<port>/<repo> --cacert ca.pem",
                            g.ca.display()
                        );
                        (g.cert, g.key)
                    }
                };
                Some(tls::server_config(&cert, &key).map_err(|e| e.to_string())?)
            } else {
                None
            };
            let htpasswd = if no_auth {
                None
            } else {
                let f = htpasswd.unwrap_or_else(|| path.join(".htpasswd"));
                if !f.exists() {
                    create_first_admin(&f)?;
                }
                Some(f)
            };
            let scheme = if tls_config.is_some() {
                "https"
            } else {
                "http"
            };
            let cfg = Config {
                path: path.clone(),
                append_only,
                private_repos,
                htpasswd: htpasswd.clone(),
                verbose,
                dashboard: !no_dashboard,
            };
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?;
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(std_listener).map_err(|e| e.to_string())?;
                eprintln!(
                    "bak-server {} serving {} on {scheme}://{}",
                    env!("CARGO_PKG_VERSION"),
                    path.display(),
                    listener.local_addr().map_err(|e| e.to_string())?
                );
                eprintln!(
                    "  auth: {}  append-only: {}  private repos: {}",
                    htpasswd
                        .as_deref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "OFF".into()),
                    on_off(append_only),
                    on_off(private_repos)
                );
                if !no_dashboard && htpasswd.is_some() {
                    eprintln!("  dashboard: {scheme}://<this host>:{}/_/", listener.local_addr().map(|a| a.port()).unwrap_or(0));
                }
                if tls_config.is_none() {
                    eprintln!("  warning: plain HTTP; use --tls unless this is behind a TLS proxy or VPN");
                }
                tokio::select! {
                    r = bak_server::serve(listener, cfg, tls_config) => r.map_err(|e| e.to_string()),
                    _ = tokio::signal::ctrl_c() => {
                        eprintln!("bak-server: shutting down");
                        Ok(())
                    }
                }
            })
        }
        Command::User(cmd) => user(cmd),
    }
}

/// No users yet: create `admin` with a generated password and print it once.
fn create_first_admin(htpasswd: &std::path::Path) -> Result<(), String> {
    let password = guard::generate_password();
    auth::set_user(htpasswd, "admin", &password)
        .map_err(|e| format!("{}: {e}", htpasswd.display()))?;
    guard::set_admin(&guard::admins_path(htpasswd), "admin", true).map_err(|e| e.to_string())?;
    eprintln!("────────────────────────────────────────────────────────────");
    eprintln!(" First start: created administrator account");
    eprintln!("   user:     admin");
    eprintln!("   password: {password}");
    eprintln!(" This is shown only once. Sign in to the dashboard and add users,");
    eprintln!(" or run `bak-server user add <name> --path <dir>`.");
    eprintln!("────────────────────────────────────────────────────────────");
    Ok(())
}

fn is_loopback(listen: &str) -> bool {
    let host = listen.rsplit_once(':').map(|(h, _)| h).unwrap_or(listen);
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn on_off(b: bool) -> &'static str {
    if b {
        "on"
    } else {
        "off"
    }
}

fn default_hostnames() -> Vec<String> {
    let mut hosts = Vec::new();
    if let Ok(out) = std::process::Command::new("hostname").output() {
        let h = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !h.is_empty() {
            hosts.push(h);
        }
    }
    hosts.push("localhost".into());
    hosts.push("127.0.0.1".into());
    hosts
}

fn user(cmd: UserCommand) -> Result<(), String> {
    match cmd {
        UserCommand::Add {
            name,
            generate,
            admin,
            file,
        } => {
            if !auth::valid_username(&name) || name.starts_with('_') {
                return Err(format!(
                    "invalid user name `{name}` (letters, digits, - _ . ; not starting with . or _)"
                ));
            }
            let path = file.resolve();
            let password = if generate {
                guard::generate_password()
            } else {
                read_password()?
            };
            auth::set_user(&path, &name, &password)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            if admin {
                guard::set_admin(&guard::admins_path(&path), &name, true)
                    .map_err(|e| e.to_string())?;
            }
            println!(
                "user {name}{} saved in {}",
                if admin { " (admin)" } else { "" },
                path.display()
            );
            if generate {
                println!("password: {password}");
                println!("(shown only once)");
            }
            Ok(())
        }
        UserCommand::Remove { name, file } => {
            let path = file.resolve();
            match auth::remove_user(&path, &name) {
                Ok(true) => {
                    let _ = guard::set_admin(&guard::admins_path(&path), &name, false);
                    println!("removed user {name} (their repositories were not touched)");
                    Ok(())
                }
                Ok(false) => Err(format!("no user {name} in {}", path.display())),
                Err(e) => Err(format!("{}: {e}", path.display())),
            }
        }
        UserCommand::List { file } => {
            let path = file.resolve();
            let text =
                std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let admins = guard::load_admins(&guard::admins_path(&path));
            let mut users: Vec<_> = auth::parse(&text).into_keys().collect();
            users.sort();
            for u in users {
                println!("{u}{}", if admins.contains(&u) { "  (admin)" } else { "" });
            }
            Ok(())
        }
    }
}

fn read_password() -> Result<String, String> {
    let password = if std::io::stdin().is_terminal() {
        let p = rpassword::prompt_password("Password: ").map_err(|e| e.to_string())?;
        let again = rpassword::prompt_password("Repeat password: ").map_err(|e| e.to_string())?;
        if p != again {
            return Err("passwords do not match".into());
        }
        p
    } else {
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        line.trim_end_matches(['\r', '\n']).to_string()
    };
    if password.chars().count() < guard::MIN_PASSWORD_LEN {
        return Err(format!(
            "password must be at least {} characters (or use --generate)",
            guard::MIN_PASSWORD_LEN
        ));
    }
    Ok(password)
}
