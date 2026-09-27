# Trip expense tracker

The design of the `trips` module: shared trip expenses, balances and
settle-up, with optional AI parsing of messages and receipts.

## Principles

- **Rust does all the maths.** Totals, splits, conversions, balances and
  settle-ups are computed by the pure core (`money`, `ledger`, `draft`); the
  AI only extracts what was written.
- **One pipeline.** Everything that logs an expense (command, AI text, receipt
  photo) produces a `Draft`; the draft card shows the computed result and
  nothing is saved until ✅.
- **Works without AI.** The AI is an optional producer of drafts, hidden when
  it isn't configured.

## Model

- **Trip**: a container with a name, a base currency, a home chat, a creator
  and a status (active, ended). A trip may have one participant (a personal
  trip) or many (split and settle-up appear then).
- **Active trip**: one per chat. A group's trip lives in the group; in a
  private chat, each user picks among the trips they belong to (`/trip use`).
  Expenses logged privately post a one-line note in the home chat
  (`notify_home_chat`).
- **Members**: named people, unique per trip, optionally linked to a Telegram
  user. People join with `/trip join`, or the creator adds them by name,
  `@username` or the user picker.
- **Permissions**: the module is `Restricted` (per-module allowed users and
  chats). Linked members may log; only the author or the trip's creator may
  edit or delete an entry. Deletion is soft; every change is audited.
- **Entries**: expenses and settlements share one ledger.
  - One currency per entry; one or more payers. The total is the sum of the
    payers' amounts (a stated total must match it).
  - Split: equal (default: every member), by shares (weights), or exact
    amounts (percentages are shares).
  - A settlement is an entry where the payer is who paid back and the share is
    who received: **a balance is always paid − share**. Spending statistics
    exclude settlements.
- **Categories**: 🍽 food, 🚕 transport, 🏨 stay, 🎟 activities, 🛍 shopping,
  💊 health, 📦 other, plus custom ones from `/config`. The AI must choose from
  the list (else "other").
- **Dates**: an entry is dated now unless given; entries may fall outside the
  trip's dates (bookings).
- **Ending**: `/trip end` freezes the trip (only settlements allowed) and posts
  a summary: total, by category, per member (paid vs share) and the
  settle-up. The creator may reopen it. `/export` sends a CSV.

## Money

- `rust_decimal::Decimal` everywhere, stored as canonical TEXT (a `Dec`
  wrapper for SeaORM: SQLite has no exact decimal type, and Rust sums
  anyway).
- Every stored amount is rounded to its currency's minor unit (ISO 4217
  table); `12.345 INR` is rejected as input.
- **Allocation** (largest remainder, in integer minor units): a total is split
  by weights; the leftover units go to the largest fractional remainders, ties
  to the earlier member. Shares always sum to the total exactly.
- **Conversion**: at logging time, the total is converted to the base currency
  (rounded once, half away from zero) with the entry's rate, which is frozen
  on the entry. Payers' and shares' base amounts are allocations of the base
  total, weighted by their amounts in the entry's currency (or the split
  weights), so they always sum to it.
- **Rates**: automatic from the ECB (frankfurter.app, cached per day), unless
  the trip has a fixed rate for the currency (e.g. a forex card) or the entry
  overrides it. Currencies the ECB lacks ask for a rate. Behind a `RateSource`
  trait.
- **Settle-up**: fewest transfers (greedy: largest debtor pays largest
  creditor, ties by member); at most n − 1 transfers. `/settle` lists them with
  ✅ *mark paid* buttons; settlements may also be logged by hand in any
  currency.

## Logging

- `/spent 2400 dinner`: payer = you, split = everyone equally, currency = the
  trip's (or the last used). Opens a **draft card**: ✏️ payers, split,
  currency, category, date, rate; ✅ saves; ❌ discards.
- Drafts are stored (`drafts` table, 24 h expiry, cleaned by a background
  task) so their buttons survive restarts; callback data is only
  `trip:d:<id>:<action>` (64-byte limit).
- Commands: `/trip` (panel: new, use, join, members, rates, end, reopen),
  `/spent`, `/balance`, `/settle`, `/export`.

## AI

- **No maths by the AI**:
  1. The extraction schema has no computed fields: amounts as written,
     currency, payers, description, category, date as written, and a split
     spec (`{equal: [..]}`, `{shares: {..}}`, `{exact: {..}}`).
  2. Every amount returned must appear literally in the user's text, or the
     draft is flagged "⚠️ couldn't verify".
  3. Receipts: the AI extracts line items and the printed total; Rust sums the
     items and shows any mismatch. Tax and tip are items.
  4. Questions: the AI picks a query from a fixed menu (`spend(by=category |
     person, range, ..)`); Rust runs it and templates every number in the
     reply.
  5. Dates: the AI copies them (ISO date, `today`, `yesterday`, a weekday);
     Rust resolves them in the bot's time zone.
  6. The trip story is prose that must contain no digits; Rust appends the
     numbers.
- **Backend**: an `Llm` trait in `src/ai/` (shared by modules). The first
  backend, `ClaudeCli`, runs the Claude Code CLI with the user's subscription:

  ```sh
  claude -p --bare --tools "" --no-session-persistence \
    --output-format json --json-schema <schema> \
    --system-prompt <prompt> --model <model>
  ```

  with the message on stdin, in an empty temporary directory, and only
  `CLAUDE_CODE_OAUTH_TOKEN` (from `claude setup-token`, valid a year) passed
  through; the reply is `structured_output`. No tools means a prompt injection
  can at worst produce a bad draft. At most 2 concurrent calls, a 60 s
  timeout, a "🤔 reading…" placeholder. Tests use `FakeLlm`.
- **Who**: `ai.users` defaults to the owner and sudo users (personal use of
  the subscription). In groups, only messages mentioning or replying to the
  bot are parsed.
- Receipt images need `--input-format stream-json` with an image block: to be
  proven by a spike before phase 3.

## Settings

- Runtime (`[modules.trips]`): `default_currency`, `notify_home_chat`,
  `custom_categories`, `ai.enabled`, `ai.users`, `ai.model`, `rates.auto`.
- File/env only: `ai.claude_path` (default: from `PATH`), `ai.oauth_token`
  (`Secret`).

## Storage

```text
trips          id, home_chat_id, name, base_currency, status, created_by, created_at, ended_at
trip_members   id, trip_id, name, user_id?          UNIQUE(trip_id, name), UNIQUE(trip_id, user_id)
trip_rates     trip_id, currency, rate
active_trips   chat_id PK, trip_id                  (a private chat's id is the user's)
entries        id, trip_id, kind, description, category, currency, total, rate,
               rate_source, base_total, spent_on, split_method, origin,
               created_by, created_at, deleted_at?, deleted_by?
entry_payers   entry_id, member_id, amount, base_amount
entry_shares   entry_id, member_id, weight?, exact?, base_amount
entry_history  id, entry_id, action, by, at, before_json
drafts         id, chat_id, message_id, created_by, json, expires_at
```

Computed base amounts are stored, so a ledger is rebuilt from stored numbers,
never recomputed at other rates.

## Code layout

```text
src/ai/                 Llm trait, AiError, FakeLlm; claude_cli.rs
src/modules/trips/
  mod.rs                Module impl, routing
  settings.rs           TripsSettings, RUNTIME_SETTINGS
  money.rs              currencies, Money, rounding, allocation      ┐
  ledger.rs             balances, settle-up                           ├ pure
  draft.rs              Draft → checked entry, date resolution        ┘
  service.rs            operations over the database, no Telegram
  command.rs            command grammar → Draft
  card.rs, panel.rs     draft card, /trip panel, callbacks
  rates.rs              RateSource, Frankfurter, cache, overrides
  extract.rs            (phase 2) schema, prompt, verification → Draft
  report.rs             summary, balances, CSV
src/db/entities/*, src/db/repositories/trips.rs, migration/m…_create_trips.rs
```

## Testing

- Properties (`proptest`): allocations sum to the total; balances sum to zero;
  settling up zeroes every balance with at most n − 1 transfers; verification
  rejects any amount absent from the text.
- Repositories and the service on `memory_db()`; the AI only through
  `FakeLlm`, plus one `#[ignore]`d test against the real CLI.

## Phases

Each phase ends with a pause for testing on Telegram.

1. **Core**
   1. `money` and `ledger`
   2. migration, entities, repositories
   3. service, drafts, `/spent`
   4. `/trip` panel, `/balance`, `/settle`
   5. ending, summary, CSV
   6. exchange rates
2. **AI text**
   1. `src/ai` with `ClaudeCli`
   2. free-text extraction and categories
3. **AI extras**
   1. receipt spike
   2. receipts
   3. questions
   4. the trip story
