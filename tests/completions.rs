use std::process::Command;

#[test]
fn completions_generate_non_empty_scripts_for_supported_shells() {
    for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
        let output = Command::new(env!("CARGO_BIN_EXE_cargo-chronoscope"))
            .args(["completions", shell])
            .output()
            .unwrap_or_else(|err| panic!("failed to run completions for {shell}: {err}"));

        assert!(
            output.status.success(),
            "completions {shell} exited with {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !output.stdout.is_empty(),
            "completions {shell} produced empty stdout"
        );
    }
}
