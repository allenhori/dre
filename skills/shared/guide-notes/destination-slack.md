- `token` is a bot token and always comes from `env_var()`, e.g.
  `"{{ env_var('SLACK_BOT_TOKEN') }}"` (SEC-3).
- Give exactly one of `channel` or `user` per entry. The bot must be invited to the channel.
- Go through the app's scopes in the docs above before the first run; a missing scope is the
  most common failure.
- Posting to a channel needs a confirmation before the run (RUN-2).
