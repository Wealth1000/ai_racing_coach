// The coach's share-bucket receiver: one endpoint, one job — accept the
// donation bundles the coach's "Send to author" POSTs (see
// `src/storage/share.rs` for the sender, and docs/neural-coach-design.md §7
// for the design). Everything here is sized for Cloudflare's free tier:
// bundles are tens of KB, so Workers KV (1 GB, 1,000 writes/day) is the
// store, with no card on file anywhere.
//
// The contract, in full:
//   GET  anything        → health check, so deployment is verifiable
//   POST anything        → the upload; validated, then stored under
//                          share/<yyyy-mm-dd>/<uuid>.json.gz
//
// Validations mirror the crate's own "refuse loudly, never guess" rule:
// wrong schema header, wrong content type, empty body, over-size body, or
// a body that is not gzip (magic bytes) are all refused with a reason —
// the sender degrades to saving the bundle on disk, so a refusal costs
// the driver nothing.

/// The bundle schema this receiver understands. Must match
/// `storage::share::SCHEMA`; a future sender is refused rather than
/// stored unreadable.
const SCHEMA = "1";

/// Uploads above this are refused without reading them. Real bundles are
/// tens of KB; the cap exists so a stray 2 GB POST cannot burn the
/// Worker's CPU time reading it.
const MAX_BYTES = 8 * 1024 * 1024;

export default {
    async fetch(request, env) {
        if (request.method === "GET") {
            return json(200, { ok: true, receiver: "coach-share", schema: SCHEMA });
        }
        if (request.method !== "POST") {
            return json(405, { error: "POST a share bundle, or GET for health" });
        }

        const schema = request.headers.get("x-coach-share-schema");
        if (schema !== SCHEMA) {
            return json(415, {
                error: `share schema ${schema ?? "(missing)"} is not ${SCHEMA}`,
            });
        }
        const type = (request.headers.get("content-type") || "").split(";")[0].trim();
        if (type !== "application/gzip") {
            return json(415, { error: `content-type '${type}' is not application/gzip` });
        }
        const declared = Number(request.headers.get("content-length") || "0");
        if (declared > MAX_BYTES) {
            return json(413, { error: `bundle is ${declared} bytes; the cap is ${MAX_BYTES}` });
        }

        // Read as a byte view, not the raw ArrayBuffer: ArrayBuffer has no
        // indexed access in JS (arr[0] is undefined), so the magic-byte
        // check below would reject every valid bundle. A Uint8Array shares
        // the buffer, costs nothing, and is what byte checks want.
        const body = new Uint8Array(await request.arrayBuffer());
        if (body.byteLength === 0) {
            return json(400, { error: "empty body" });
        }
        if (body.byteLength > MAX_BYTES) {
            return json(413, { error: `bundle is ${body.byteLength} bytes; the cap is ${MAX_BYTES}` });
        }
        // The sender's own plausibility check, one level up: a gzip stream
        // starts 1f 8b. Anything else is not a bundle this coach wrote.
        if (body[0] !== 0x1f || body[1] !== 0x8b) {
            return json(415, { error: "body does not look like gzip" });
        }

        const day = new Date().toISOString().slice(0, 10);
        const key = `share/${day}/${crypto.randomUUID()}.json.gz`;
        await env.SHARES.put(key, body, {
            metadata: { schema: SCHEMA, bytes: body.byteLength },
        });
        return json(200, { ok: true, key });
    },
};

function json(status, value) {
    return new Response(JSON.stringify(value), {
        status,
        headers: { "content-type": "application/json" },
    });
}
