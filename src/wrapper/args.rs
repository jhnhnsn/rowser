//! The ssh destination in an argv, for when `ssh -G` cannot resolve it.

use std::ffi::OsString;

/// ssh option letters that take a value (from `ssh(1)`).
const WITH_VALUE: &str = "BbcDEeFIiJLlmOoPpQRSWw";

/// The Host alias in ssh arguments: the first argument that is not an option
/// or an option's value, without any `user@` prefix or `ssh://` URI form.
pub fn destination(args: &[OsString]) -> Option<String> {
    let mut iter = args.iter().map(|a| a.to_string_lossy());
    while let Some(arg) = iter.next() {
        if arg == "--" {
            return iter.next().and_then(|d| host_of(&d));
        }
        if arg.len() > 1 && arg.starts_with('-') {
            for (i, c) in arg.char_indices().skip(1) {
                if WITH_VALUE.contains(c) {
                    if i + c.len_utf8() == arg.len() {
                        iter.next();
                    }
                    break;
                }
            }
            continue;
        }
        return host_of(&arg);
    }
    None
}

fn host_of(dest: &str) -> Option<String> {
    let rest = match dest.strip_prefix("ssh://") {
        Some(uri) => {
            let authority = uri.split('/').next().unwrap_or("");
            let host_port = authority.rsplit('@').next().unwrap_or("");
            match host_port.strip_prefix('[') {
                Some(v6) => v6.split(']').next().unwrap_or(""),
                None => host_port.split(':').next().unwrap_or(""),
            }
        }
        None => dest.rsplit('@').next().unwrap_or(""),
    };
    (!rest.is_empty()).then(|| rest.to_string())
}

#[cfg(test)]
mod tests {
    use super::destination;

    fn dest(args: &[&str]) -> Option<String> {
        let args: Vec<_> = args.iter().map(Into::into).collect();
        destination(&args)
    }

    #[test]
    fn bare_host() {
        assert_eq!(dest(&["myhost"]).as_deref(), Some("myhost"));
    }

    #[test]
    fn user_at_host() {
        assert_eq!(dest(&["alice@myhost"]).as_deref(), Some("myhost"));
    }

    #[test]
    fn options_with_separate_values_are_skipped() {
        let args = [
            "-p", "2222", "-i", "/k", "-o", "User=x", "-J", "jump", "-l", "bob", "myhost",
        ];
        assert_eq!(dest(&args).as_deref(), Some("myhost"));
    }

    #[test]
    fn options_with_attached_values_are_skipped() {
        assert_eq!(
            dest(&["-p2222", "-oUser=x", "-Jjump", "myhost"]).as_deref(),
            Some("myhost")
        );
    }

    #[test]
    fn flag_clusters_ending_in_a_value_option() {
        assert_eq!(
            dest(&["-At", "-vp", "22", "-4", "myhost"]).as_deref(),
            Some("myhost")
        );
    }

    #[test]
    fn trailing_remote_command_is_ignored() {
        assert_eq!(
            dest(&["-t", "u@myhost", "tmux", "-u", "attach"]).as_deref(),
            Some("myhost")
        );
    }

    #[test]
    fn double_dash_ends_options() {
        assert_eq!(
            dest(&["-v", "--", "myhost", "ls"]).as_deref(),
            Some("myhost")
        );
    }

    #[test]
    fn ssh_uri() {
        assert_eq!(dest(&["ssh://u@myhost:2222"]).as_deref(), Some("myhost"));
        assert_eq!(dest(&["ssh://[::1]:22"]).as_deref(), Some("::1"));
    }

    #[test]
    fn no_destination() {
        assert_eq!(dest(&["-v", "-p", "22"]), None);
    }
}
