# Cloud partner developer limits

What each cloud provider limits on OUR side of the integration: the API
projects and OAuth clients this app is registered under. These are partner and
developer limits, not end-user storage limits. Everything here was verified
against the providers' documentation on 2026-08-04; providers move these
numbers, so re-verify before relying on one for a release decision.

## Google Drive

- The `drive.file` scope is classified non-sensitive. No verification review
  is required, users see no unverified-app warning, and the 100-user cap that
  unverified sensitive scopes carry does not apply to Drive-only use.
- Quota, under the model that took effect 2026-05-01 for new projects: 400
  million quota units per day free per project, with a file create costing 5
  units. Per-minute ceilings are 1,000,000 units per project and 325,000 per
  user. Beyond the free tier Google plans billing, with pricing promised later
  in 2026 on 90 days notice. In practice uploads are unlimited for our scale.
- Google itself caps each user at 750 GB uploaded per day across My Drive and
  shared drives. That applies to every client equally, not just ours.
- An OAuth consent screen in Testing status allows at most 100 test users and
  expires refresh tokens after 7 days. Flipping to production clears both, and
  with only non-sensitive scopes it is just a click, no review.

## YouTube (the same Google project, different rules)

- The upload scope is sensitive. A published app requesting it shows the
  unverified-app warning until it passes Google's OAuth verification, and
  unverified sensitive-scope use carries a 100-user cap.
- Forced private: Google documents that videos uploaded via `videos.insert`
  from API projects created after 2020-07-28 that have not passed the YouTube
  compliance audit get locked to private, unappealably; the only recourse is
  re-uploading outside the API. The policy was still on the live docs in
  August 2026, but in practice it did not fire on a fresh unaudited 2026
  project (an unlisted upload stayed unlisted). Treat it as dormant but real:
  it can start firing again without notice.
- Quota: a December 2025 change dropped the upload cost from about 1,600 units
  to about 100, and a June 2026 change gave `videos.insert` its own bucket.
  The default is now 100 uploads per day, separate from the 10,000-unit
  general pool. `videos.delete` (our undo and cancel path) costs 50 units from
  the general pool.
- Larger quotas and the compliance audit both go through the YouTube API
  audit and quota extension form.

## Microsoft OneDrive (Graph)

- No daily quotas and no user caps. Microsoft throttles dynamically: exceed a
  threshold and requests get HTTP 429 with a Retry-After header to honor.
- Unverified publisher: personal accounts see an "unverified" note on the
  consent screen but connect fine (the app's own 2026 registration proved it).
  Work and school users in OTHER tenants are BLOCKED from consenting by
  default: Microsoft's risk-based step-up consent refuses unverified
  multitenant apps registered after 2020-11-08 that ask beyond basic sign-in,
  and only tenant-admin consent or publisher verification lifts it. Publisher
  verification requires a Partner Center (MPN) account with legal-business
  vetting; skipped at launch, revisit if a corporate audience appears. The
  publisher DOMAIN (the .well-known JSON on thedragon.dev) is set separately
  and only fills the consent screen's domain line.
- Upload session URLs are handed out on shifting host families; the accepted
  list lives in `is_upload_session_host` in `src/cloud/providers/onedrive.rs`,
  and a refused host is logged by name so a new legitimate one is
  self-diagnosing.

## Dropbox

- Apps start in development status: at most 500 linked users, and, sharper
  than that, once 50 users have linked the app there is a two-week window to
  apply for and receive production status before new linking freezes.
- Production status is an approval review against Dropbox's branding
  guidelines. Action item: apply for production before or immediately after
  the first public release that bakes the Dropbox client id.
- Rate limiting is per-user request throttling (429s). No daily quotas.

## Sources

- Google Drive scopes: <https://developers.google.com/workspace/drive/api/guides/api-specific-auth>
- Google Drive usage limits: <https://developers.google.com/workspace/drive/api/guides/limits>
- YouTube videos.insert (forced-private note): <https://developers.google.com/youtube/v3/docs/videos/insert>
- YouTube quota and audits: <https://developers.google.com/youtube/v3/guides/quota_and_compliance_audits>
- YouTube revision history (Dec 2025 and Jun 2026 quota changes): <https://developers.google.com/youtube/v3/revision_history>
- Videos locked as private (user-facing help): <https://support.google.com/youtube/answer/7300965>
- Microsoft Graph throttling: <https://learn.microsoft.com/en-us/graph/throttling-limits>
- Dropbox developer guide (development vs production status): <https://www.dropbox.com/developers/reference/developer-guide>
