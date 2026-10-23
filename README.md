# its4tasks

A personal todo board: up to four tasks in focus as cards, the rest in a list, and a log of what happened each day or week. Each task keeps its comments as a running log, with next steps, blockers and stars. Hence the name, read either way: it's for tasks, and it's four tasks.

Boards are personal. Everyone who signs in gets their own board, and nothing is shared between users.

its4tasks runs in two ways:

- **On your computer only.** The desktop app keeps a local board in a file. There's no server and no account.
- **On a server.** `its4tasks-server` keeps every user's board and serves the app to browsers. The desktop app can connect to it too. People sign in through an outside sign-in service such as Clerk.

## What you need

- Rust (stable), with `cargo`.
- For the web app: the WASM target and [trunk](https://trunkrs.dev):
  ```sh
  rustup target add wasm32-unknown-unknown
  cargo install trunk
  ```

## On your computer

```sh
cargo run --release -p its4tasks
```

To install it as the `its4tasks` command instead:

```sh
cargo install --path crates/gui
```

The first time it starts, the app asks where your board is. Choose **Use a local board**. The board is kept in `~/.local/share/its4tasks/board.sqlite` on Linux, or the matching data directory on other systems. If you used an older version, the board you had there is still yours.

The app remembers the choice in `~/.config/its4tasks/config.json`. To change it later, use **Switch board…** in the **⋯** menu above the task list.

To get the board out from a script, `its4tasks --get-board` prints it to standard output as an `its4tasks.json` (the same file **Export** saves) and opens no window:

```sh
its4tasks --get-board > board.json
```

It reads whichever board the app last opened, local or on a server. For a server, the app must have signed in once. If the server's sign-in service replaces refresh tokens each time they're used, running it while the app is open on that server makes the app ask you to sign in again.

## On a server

### Build

The server has the web app built into it, so build the web app first:

```sh
(cd crates/gui && trunk build --release)
cargo build --release -p its4tasks-server                                              # SQLite
cargo build --release -p its4tasks-server --no-default-features --features postgres    # Postgres
```

Both features can be on at once. The result is `target/release/its4tasks-server`, a single binary that needs nothing else.

Or build a container image with the `Dockerfile`. It builds both the web app and the server, for Postgres by default:

```sh
docker build -t its4tasks .                             # Postgres
docker build --build-arg FEATURES=sqlite -t its4tasks . # SQLite
```

The container listens on `$PORT` (8080 if unset) unless `ITS4TASKS_ADDR` says otherwise.

### Configure

Everything is set through environment variables.

| Variable | Meaning |
|---|---|
| `DATABASE_URL` | Where boards are kept, e.g. `sqlite:///var/lib/its4tasks/boards.sqlite?mode=rwc` or `postgres://user:password@host/its4tasks`. The default is `server.sqlite` in the data directory. Tables are created and migrated on start. |
| `ITS4TASKS_ADDR` | The address to listen on. The default is `127.0.0.1:7878`. |
| `ITS4TASKS_PUBLIC_URL` | The address people reach the server at, e.g. `https://tasks.example.com`. It's used to send them back after they sign in. Without it, the server uses the `Host` and `X-Forwarded-Proto` headers. |
| `ITS4TASKS_CLERK_PUBLISHABLE_KEY` | Sign in with Clerk, see below. With it, the two settings after this one are optional. |
| `ITS4TASKS_AUTH_ISSUER` | The sign-in service's issuer, the `iss` of its tokens. **Required** without Clerk. |
| `ITS4TASKS_AUTH_SIGN_IN_URL` | Its sign-in page. `{redirect}` is replaced with the page to come back to, e.g. `https://accounts.example.com/sign-in?redirect_url={redirect}`. **Required** without Clerk. |
| `ITS4TASKS_AUTH_JWKS_URL` | Its public keys. The default is `{issuer}/.well-known/jwks.json`. |
| `ITS4TASKS_AUTH_AUDIENCE` | If set, tokens must have this `aud`. |
| `ITS4TASKS_AUTH_AUTHORIZED_PARTIES` | If set, a comma-separated list of the sites (e.g. `https://its4tasks.example.com`) tokens must name in `azp`, as Clerk's do. Set this or the audience if the issuer serves other apps too: otherwise their users' tokens sign in here as well. The server warns at startup when neither is set. |
| `ITS4TASKS_AUTH_COOKIE` | The cookie browsers carry the token in. The default is `__session`. |
| `ITS4TASKS_OAUTH_AUTHORIZE_URL`, `ITS4TASKS_OAUTH_TOKEN_URL`, `ITS4TASKS_OAUTH_CLIENT_ID` | Optional, all three or none: lets the desktop app sign in, see below. |

Put the server behind a reverse proxy that serves HTTPS, and keep `ITS4TASKS_ADDR` on a local address.

### Signing in

its4tasks has no passwords of its own. Sign-in is the job of the outside service, which must provide:

- **Signed JWTs** (RS256, ES256 or EdDSA). The keys are published as a JWKS, and the token has `iss`, `sub` and `exp`. A user is their `sub` at that `iss`. The first time the server sees one, it creates the user, who then chooses to start a new board or import an `its4tasks.json`.
- **A sign-in page** that sends people back to `{redirect}`, with the token in the `ITS4TASKS_AUTH_COOKIE` cookie on the server's own domain. API requests also accept the token as `Authorization: Bearer <token>`.
- **OAuth 2 for the desktop app** (optional). The app needs the authorization code flow, for a public client (no secret), with PKCE (S256) and refresh tokens. It must be allowed to redirect to `http://127.0.0.1:<any port>/callback`, because the app listens on a random local port while you sign in in your browser.

A page request without a valid token is sent to the sign-in page. An API request without one gets `401`, and one that touches another user's tasks gets `403`.

### Signing in with Clerk

[Clerk](https://clerk.com) session tokens last a minute and are kept fresh by Clerk's own script in the page. Set `ITS4TASKS_CLERK_PUBLISHABLE_KEY` to your Clerk application's publishable key (`pk_live_…` or `pk_test_…`), and the server adds that script to the web app's page. The web app then asks it for a fresh token before each request and sends it as `Authorization: Bearer`. Someone who isn't signed in is sent to your Clerk sign-in page, and comes back afterwards.

- The issuer defaults to the Clerk Frontend API named in the key, so `ITS4TASKS_AUTH_ISSUER` and `ITS4TASKS_AUTH_SIGN_IN_URL` aren't needed.
- Clerk's production keys only work on the domain the Clerk application is set up for, or its subdomains. Serve its4tasks there, or set its domain up in Clerk as a satellite.
- With Clerk, the server serves the page to anyone, because the page itself handles sign-in. The page holds no data: everything comes through the API, which still needs a valid token.
- `ITS4TASKS_CLERK_FRONTEND_API` overrides the Frontend API's address. It's meant for testing.

So far this is tested against a stand-in for Clerk's script that `its4tasks-dev-auth` serves, built to match what a real Clerk app sends. If the server refuses Clerk's tokens, its log says why (`refused a token: …`), and so does the "Signed out" page.

If you're signed out while using the app, your unsent changes and drafts are kept in the browser and come back after you sign in again.

### Deploying on Railway with Clerk

1. Create a project with a Postgres database and a service from this repository. Railway finds the `Dockerfile` and builds it.
2. Set the service's variables:
   - `ITS4TASKS_CLERK_PUBLISHABLE_KEY`: your Clerk publishable key.
   - `DATABASE_URL`: `${{Postgres.DATABASE_URL}}`, a reference to the database's connection string.
3. Generate a domain for the service, or add your own. A `pk_live_` key needs a domain under the one your Clerk production app is set up for. A `pk_test_` key works on the `*.up.railway.app` address.

Railway sets `PORT` and the container listens on it. For SQLite instead of Postgres, build with `FEATURES=sqlite` (a build argument, set as a variable of the same name). Then mount a volume, say at `/data`, and set `DATABASE_URL=sqlite:///data/boards.sqlite?mode=rwc`. Railway mounts volumes owned by root and the container runs as a regular user, so also set `RAILWAY_RUN_UID=0`.

### Connect the desktop app

Start the app and enter the server's address under **Or connect to a server**. Your browser opens the sign-in page. After you sign in, the app shows your board and keeps you signed in with a refresh token, which it stores in its config file (readable only by you).

## Trying the server locally

`its4tasks-dev-auth` is a stand-in sign-in service: anyone can sign in as anyone by typing a name. It signs tokens with a key that is public in this repository, so **never use it, or that key, for anything real.**

```sh
(cd crates/gui && trunk build)
cargo build
./target/debug/its4tasks-dev-auth &          # on 127.0.0.1:7980
ITS4TASKS_ADDR=127.0.0.1:7979 \
ITS4TASKS_AUTH_ISSUER=http://127.0.0.1:7980 \
ITS4TASKS_AUTH_SIGN_IN_URL='http://127.0.0.1:7980/sign-in?redirect={redirect}' \
ITS4TASKS_OAUTH_AUTHORIZE_URL=http://127.0.0.1:7980/authorize \
ITS4TASKS_OAUTH_TOKEN_URL=http://127.0.0.1:7980/token \
ITS4TASKS_OAUTH_CLIENT_ID=its4tasks \
DATABASE_URL="sqlite://$(mktemp -d)/server.sqlite?mode=rwc" \
./target/debug/its4tasks-server
```

Open <http://127.0.0.1:7979/> and sign in with any name. A second browser profile, signed in with another name, gets a separate board. The desktop app can connect to `http://127.0.0.1:7979` the same way.

dev-auth also stands in for Clerk's script, to try the Clerk setup. Start the server with these settings instead of the `ITS4TASKS_AUTH_*` ones. The key is `127.0.0.1:7980$` in base64.

```sh
ITS4TASKS_CLERK_PUBLISHABLE_KEY=pk_test_MTI3LjAuMC4xOjc5ODAk \
ITS4TASKS_CLERK_FRONTEND_API=http://127.0.0.1:7980
```

Starting dev-auth with `ITS4TASKS_DEV_AUTH_TTL=15` makes its tokens expire quickly, to watch the page renew them.

## Exporting and importing

**Export its4tasks.json…** in the **⋯** menu above the task list saves your whole board to a file: every task, comment and focus card. In the browser it downloads as `its4tasks.json`.

**Import its4tasks.json…** in the same menu adds every task in such a file to your board, with new ids, so a board can be moved between servers or into a local one. `board.json` files from older versions of the app work too. New users are offered the same on the first page they see: start a new board, or import one.

## Working on it

`AGENTS.md` describes how the code is laid out, the checks to run before a change, and how to test the app on screen without touching your own data.
