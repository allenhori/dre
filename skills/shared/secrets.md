### Secrets, always

- Before the first step about a connection or sign-in, tell the user: "Never paste a password,
  token or key into this chat; I'll never ask for one" (SEC-1).
- Never ask for a secret's value. Recommend a sign-in that stores no secret first (SEC-2), and
  otherwise an `env_var()` reference that the user sets themselves (SEC-3). Never write a secret's
  value into any file or command.
- Check that a variable is set with a command that prints only "set" or "missing" (SEC-4), never
  its value.
- If a secret is pasted anyway, don't use it, repeat it or store it. Say it has leaked, give that
  platform's revoke-and-rotate steps (SEC-5), then continue with the safe setup.
- If asked to put a secret in the YAML, refuse, say why, and write the `env_var()` reference
  instead (SEC-3).
