pub const DEFAULT_SERVER_URL: &str = "http://127.0.0.1:8080";
pub const ENV_URL: &str = "LLAMA_SERVER_URL";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliConfig {
    pub server_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliAction {
    Run(CliConfig),
    Help,
}

pub fn normalize_url(input: &str) -> String {
    let trimmed = input.trim();
    let without_trailing = trimmed.trim_end_matches('/');
    if without_trailing.starts_with("http://") || without_trailing.starts_with("https://") {
        without_trailing.to_string()
    } else {
        format!("http://{without_trailing}")
    }
}

pub fn help_message() -> String {
    format!(
        "Usage: llama-dbusd [OPTIONS]\n\n\
An ultra-lightweight, asynchronous Rust D-Bus daemon for local llama.cpp orchestration.\n\n\
Options:\n\
  -u, --url <URL>    llama-server base URL [default: {DEFAULT_SERVER_URL}] [env: {ENV_URL}]\n\
  -h, --help         Print help information\n"
    )
}

pub fn parse_args<I, T>(args: I) -> Result<CliAction, String>
where
    I: IntoIterator<Item = T>,
    T: Into<String>,
{
    let env_val = std::env::var(ENV_URL).ok();
    parse_args_internal(args, env_val.as_deref())
}

pub fn parse_args_internal<I, T>(args: I, env_url: Option<&str>) -> Result<CliAction, String>
where
    I: IntoIterator<Item = T>,
    T: Into<String>,
{
    let args: Vec<String> = args.into_iter().map(Into::into).collect();
    let mut url = None;
    let mut show_help = false;
    let mut i = 0;
    let mut in_options = true;

    while i < args.len() {
        let arg = &args[i];

        if in_options && arg == "--" {
            in_options = false;
            i += 1;
            continue;
        }

        if in_options && arg.starts_with("--") {
            if arg == "--help" {
                show_help = true;
            } else if arg == "--url" {
                i += 1;
                if i >= args.len() {
                    return Err(
                        "llama-dbusd: option '--url' requires an argument\nTry 'llama-dbusd --help' for more information.".to_string(),
                    );
                }
                url = Some(args[i].clone());
            } else if let Some(val) = arg.strip_prefix("--url=") {
                if val.is_empty() {
                    return Err(
                        "llama-dbusd: option '--url' requires an argument\nTry 'llama-dbusd --help' for more information.".to_string(),
                    );
                }
                url = Some(val.to_string());
            } else {
                return Err(format!(
                    "llama-dbusd: unrecognized option '{arg}'\nTry 'llama-dbusd --help' for more information."
                ));
            }
        } else if in_options && arg.starts_with('-') && arg != "-" {
            let chars: Vec<char> = arg[1..].chars().collect();
            for (j, &c) in chars.iter().enumerate() {
                match c {
                    'h' => {
                        show_help = true;
                    }
                    'u' => {
                        if j + 1 < chars.len() {
                            let rest: String = chars[j + 1..].iter().collect();
                            url = Some(rest);
                            break;
                        } else {
                            i += 1;
                            if i >= args.len() {
                                return Err(
                                    "llama-dbusd: option requires an argument -- 'u'\nTry 'llama-dbusd --help' for more information.".to_string(),
                                );
                            }
                            url = Some(args[i].clone());
                            break;
                        }
                    }
                    other => {
                        return Err(format!(
                            "llama-dbusd: invalid option -- '{other}'\nTry 'llama-dbusd --help' for more information."
                        ));
                    }
                }
            }
        } else {
            return Err(format!(
                "llama-dbusd: unexpected argument '{arg}'\nTry 'llama-dbusd --help' for more information."
            ));
        }

        i += 1;
    }

    if show_help {
        return Ok(CliAction::Help);
    }

    let raw_url = if let Some(u) = url {
        u
    } else if let Some(env_u) = env_url {
        let trimmed = env_u.trim();
        if !trimmed.is_empty() {
            trimmed.to_string()
        } else {
            DEFAULT_SERVER_URL.to_string()
        }
    } else {
        DEFAULT_SERVER_URL.to_string()
    };

    let normalized = normalize_url(&raw_url);
    Ok(CliAction::Run(CliConfig {
        server_url: normalized,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_url() {
        assert_eq!(
            normalize_url("http://127.0.0.1:8080"),
            "http://127.0.0.1:8080"
        );
        assert_eq!(
            normalize_url("http://127.0.0.1:8080/"),
            "http://127.0.0.1:8080"
        );
        assert_eq!(
            normalize_url("https://127.0.0.1:8080/"),
            "https://127.0.0.1:8080"
        );
        assert_eq!(normalize_url("127.0.0.1:8080"), "http://127.0.0.1:8080");
        assert_eq!(normalize_url("127.0.0.1:8080/"), "http://127.0.0.1:8080");
        assert_eq!(normalize_url("  localhost:8080  "), "http://localhost:8080");
    }

    #[test]
    fn test_default_config_when_no_args_and_no_env() {
        let res = parse_args_internal(Vec::<String>::new(), None).unwrap();
        assert_eq!(
            res,
            CliAction::Run(CliConfig {
                server_url: DEFAULT_SERVER_URL.to_string()
            })
        );
    }

    #[test]
    fn test_env_var_when_no_cli_args() {
        let res = parse_args_internal(Vec::<String>::new(), Some("http://0.0.0.0:9090")).unwrap();
        assert_eq!(
            res,
            CliAction::Run(CliConfig {
                server_url: "http://0.0.0.0:9090".to_string()
            })
        );
    }

    #[test]
    fn test_empty_env_var_falls_back_to_default() {
        let res = parse_args_internal(Vec::<String>::new(), Some("   ")).unwrap();
        assert_eq!(
            res,
            CliAction::Run(CliConfig {
                server_url: DEFAULT_SERVER_URL.to_string()
            })
        );
    }

    #[test]
    fn test_short_flag_space() {
        let res = parse_args_internal(["-u", "http://127.0.0.1:8081"], None).unwrap();
        assert_eq!(
            res,
            CliAction::Run(CliConfig {
                server_url: "http://127.0.0.1:8081".to_string()
            })
        );
    }

    #[test]
    fn test_short_flag_adjacent() {
        let res = parse_args_internal(["-uhttp://127.0.0.1:8081"], None).unwrap();
        assert_eq!(
            res,
            CliAction::Run(CliConfig {
                server_url: "http://127.0.0.1:8081".to_string()
            })
        );
    }

    #[test]
    fn test_long_flag_space() {
        let res = parse_args_internal(["--url", "http://127.0.0.1:8081"], None).unwrap();
        assert_eq!(
            res,
            CliAction::Run(CliConfig {
                server_url: "http://127.0.0.1:8081".to_string()
            })
        );
    }

    #[test]
    fn test_long_flag_equal() {
        let res = parse_args_internal(["--url=http://127.0.0.1:8081"], None).unwrap();
        assert_eq!(
            res,
            CliAction::Run(CliConfig {
                server_url: "http://127.0.0.1:8081".to_string()
            })
        );
    }

    #[test]
    fn test_cli_overrides_env_var() {
        let res = parse_args_internal(["-u", "http://cli-addr:8080"], Some("http://env-addr:8080"))
            .unwrap();
        assert_eq!(
            res,
            CliAction::Run(CliConfig {
                server_url: "http://cli-addr:8080".to_string()
            })
        );
    }

    #[test]
    fn test_help_short_and_long() {
        assert_eq!(parse_args_internal(["-h"], None).unwrap(), CliAction::Help);
        assert_eq!(
            parse_args_internal(["--help"], None).unwrap(),
            CliAction::Help
        );
    }

    #[test]
    fn test_bundled_help_flag() {
        assert_eq!(
            parse_args_internal(["-hu", "http://127.0.0.1:8081"], None).unwrap(),
            CliAction::Help
        );
    }

    #[test]
    fn test_end_of_options_delimiter() {
        let res = parse_args_internal(["-u", "http://127.0.0.1:8081", "--"], None).unwrap();
        assert_eq!(
            res,
            CliAction::Run(CliConfig {
                server_url: "http://127.0.0.1:8081".to_string()
            })
        );
    }

    #[test]
    fn test_positional_arg_after_delimiter_errors() {
        let err = parse_args_internal(["--", "extra"], None).unwrap_err();
        assert!(err.contains("unexpected argument 'extra'"));
    }

    #[test]
    fn test_unexpected_positional_argument() {
        let err = parse_args_internal(["positional"], None).unwrap_err();
        assert!(err.contains("unexpected argument 'positional'"));
    }

    #[test]
    fn test_missing_argument_short() {
        let err = parse_args_internal(["-u"], None).unwrap_err();
        assert!(err.contains("option requires an argument -- 'u'"));
    }

    #[test]
    fn test_missing_argument_long() {
        let err = parse_args_internal(["--url"], None).unwrap_err();
        assert!(err.contains("option '--url' requires an argument"));

        let err_eq = parse_args_internal(["--url="], None).unwrap_err();
        assert!(err_eq.contains("option '--url' requires an argument"));
    }

    #[test]
    fn test_invalid_short_option() {
        let err = parse_args_internal(["-x"], None).unwrap_err();
        assert!(err.contains("invalid option -- 'x'"));
    }

    #[test]
    fn test_unrecognized_long_option() {
        let err = parse_args_internal(["--foo"], None).unwrap_err();
        assert!(err.contains("unrecognized option '--foo'"));
    }

    #[test]
    fn test_help_message_content() {
        let msg = help_message();
        assert!(msg.contains("Usage: llama-dbusd [OPTIONS]"));
        assert!(msg.contains("-u, --url <URL>"));
        assert!(msg.contains("-h, --help"));
        assert!(msg.contains("http://127.0.0.1:8080"));
        assert!(msg.contains("LLAMA_SERVER_URL"));
    }
}
