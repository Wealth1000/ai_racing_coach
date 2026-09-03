# The share receiver — Cloudflare Worker + KV

The bucket end of the sharing programme (`docs/neural-coach-design.md` §7):
drivers who opted in press "Send to author", the coach POSTs a gzipped
bundle (scrubbed session names + a small manifest — see
`src/storage/share.rs`), and this Worker stores it. One endpoint, one
store, nothing else.

**Why KV and not R2:** Workers KV's free tier is 1 GB storage and 1,000
writes/day with **no card on file anywhere**. A bundle is 10–100 KB of
gzipped CSV, so 1 GB is thousands of donations. R2's free tier is bigger
(10 GB, no egress fees) but requires adding a payment method to activate —
when donations ever outgrow KV, the switch is three lines (see the bottom
of this file).

## Path A — with Node installed

    cd share-backend
    npm install                                  # installs wrangler locally
    npx wrangler login                           # opens the browser once

Create the KV namespace (the free tier needs no billing setup):

    npx wrangler kv namespace create SHARES

It prints a binding block — paste the `id` into `wrangler.jsonc` where
`PASTE_THE_NAMESPACE_ID_HERE` stands, then:

    npx wrangler deploy

It prints the URL: `https://coach-share.<your-subdomain>.workers.dev`.

## Path B — no local tooling at all (browser only)

1. **Create the Worker:** dash.cloudflare.com → Workers & Pages →
   *Create application* → *Create Worker* → name it `coach-share` →
   *Deploy*, then *Edit code*, paste the whole of `src/worker.js`, deploy
   again.
2. **Create the namespace:** Storage & Databases → KV → *Create
   namespace*, name it `SHARES`.
3. **Bind them:** the Worker's *Settings* → *Bindings* → *Add* → KV:
   variable name `SHARES` (exactly — the code reads `env.SHARES`), the
   namespace from step 2. Deploy the Worker once more if it asks.

No file in this folder is needed for this path except `src/worker.js` —
`wrangler.jsonc` and `package.json` only exist for Path A.

## Turn the client on

Nothing to turn on: the coach ships with this receiver's URL compiled in
(`storage::share::DEFAULT_ENDPOINT`), so a consenting driver's "Send to
author" just works. `COACH_SHARE_ENDPOINT` still *overrides* it — set it
to a throwaway receiver when testing, or to your own bucket in a fork.

A Send whose upload fails (receiver down, network gone) never blocks or
errors the driver: the bundle is written to `data/share/` to send by
hand, and the job screen says where.

## Verify it works

Health check (GET):

    curl https://coach-share.<your-subdomain>.workers.dev
    # → {"ok":true,"receiver":"coach-share","schema":"1"}

A real upload, exactly as the coach sends it (any bundle under
`data/share/` from a Send without the env var):

    ./test-upload.sh https://coach-share.<your-subdomain>.workers.dev \
        data/share/share_<track>_<stamp>.json.gz
    # → {"ok":true,"key":"share/2026-09-03/<uuid>.json.gz"}

Then delete the test bundle from KV so it never mixes into the corpus.

## Getting the donations back out

    npx wrangler kv key list --namespace-id <id> --prefix share/ --remote
    npx wrangler kv key get share/2026-09-03/<uuid>.json.gz \
        --namespace-id <id> --remote --path donated.json.gz

(Flags drift between wrangler versions; `npx wrangler kv --help` is the
truth.) The dashboard also lists and previews keys. Every bundle is
readable with the crate itself: `storage::share::read_bundle` parses the
manifest and CSV back out, so ingesting donations into the training
corpus is one function call per file.

## Limits and abuse, honestly

The only gates are the schema header, the content type, the gzip magic
bytes, and the 8 MB cap. A determined stranger could burn the 1,000
writes/day quota with garbage-but-gzip bodies; the worst case is a day
with no donations stored, not a bill. If that ever happens, add a shared
secret header to the Worker and to `share::upload` (the Rust side gains
one `.set(...)` line).

## Switching to R2 later

When the corpus outgrows 1 GB or 1,000 uploads/day: create an R2 bucket
(`npx wrangler r2 bucket create coach-share` — this is the step that asks
for a payment method), then in `wrangler.jsonc` replace the
`kv_namespaces` block with:

    "r2_buckets": [{ "binding": "SHARES", "bucket_name": "coach-share" }]

and in `src/worker.js` drop the `metadata` option from the `.put()` call
(R2 ignores it; KV is where it matters). Nothing else changes — the key
shape, the validations, and the client are identical.
