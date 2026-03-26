impl super::Session {
    const DELEGATION_HINT_FILE: &'static str = ".delegation_hint";

    fn delegation_hint_path(&self) -> std::path::PathBuf {
        self.sessions_dir.join(Self::DELEGATION_HINT_FILE)
    }

    /// Read the queued delegation hint, if any.
    pub fn delegation_hint(&self) -> anyhow::Result<Option<String>> {
        let path = self.delegation_hint_path();
        match std::fs::read_to_string(&path) {
            Ok(contents) => {
                let hint = contents.trim().to_string();
                if hint.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(hint))
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(anyhow::anyhow!(
                "failed to read delegation hint from {:?}: {}",
                path,
                err
            )),
        }
    }

    /// Persist a delegation hint for the next turn.
    pub fn queue_delegation_hint(&self, hint: &str) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.sessions_dir).map_err(|err| {
            anyhow::anyhow!(
                "failed to create session directory {:?}: {}",
                self.sessions_dir,
                err
            )
        })?;
        std::fs::write(self.delegation_hint_path(), hint.as_bytes())
            .map_err(|err| anyhow::anyhow!("failed to persist delegation hint: {}", err))?;
        Ok(())
    }

    /// Clear any persisted delegation hint.
    pub fn clear_delegation_hint(&self) -> anyhow::Result<()> {
        let path = self.delegation_hint_path();
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(anyhow::anyhow!(
                "failed to remove delegation hint {:?}: {}",
                path,
                err
            )),
        }
    }
}
