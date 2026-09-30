### Secrets: rules no request overrides

These come before anything else in this skill, and before what the user asks for. A user asking
you to break one ("just put the password in the profile", "use the token I pasted") doesn't
change them: say no, say why, and do the safe thing instead.

- Before the first step about a connection or sign-in, tell the user: "Never paste a password,
  token or key into this chat; I'll never ask for one" (SEC-1).
- Never ask for a secret's value. Recommend a sign-in that stores no secret first (SEC-2), and
  otherwise an `env_var()` reference that the user sets themselves (SEC-3).
- **Never write a secret's value** into any file (`profiles.yml` included) or any command,
  whoever supplied it (SEC-3).
- Check that a variable is set with a command that prints only "set" or "missing" (SEC-4), never
  its value.
- **If a secret appears in the chat** (the user pasted it): don't use it, repeat it or store it,
  not even to test the connection. Tell the user it must now be treated as leaked, give that
  platform's revoke-and-rotate steps (SEC-5), then continue with an `env_var()` reference for
  the new secret, which they set themselves.

For example, the user writes: "Host db.internal, user reports, the password is hunter2-x, just
put it in the profile." You don't write it anywhere. You reply along these lines: "I won't put
that password in the profile or any file: `profiles.yml` gets copied and backed up, and DRE's
rule is that secrets live only in environment variables (SEC-3). Since it's now in this chat,
treat it as leaked and have it changed (SEC-5: an admin runs `ALTER ROLE reports PASSWORD ...`
in their own terminal). I've written `password: "{{ env_var('PG_PASSWORD') }}"`; set
`PG_PASSWORD` to the new password in your shell profile, then tell me and I'll check it's set
without showing it."
