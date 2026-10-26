// Entry point so that `node --test tests/` works on Node 24, which resolves a
// directory positional through normal module resolution instead of expanding it.
// `node --test` (auto-discovery) and `node --test tests/*.test.mjs` work too.
import './server.test.mjs';
import './config.test.mjs';
import './api.test.mjs';
import './transcript.test.mjs';
import './spawn.test.mjs';
import './kill.test.mjs';
import './autoname.test.mjs';
import './snapshot.test.mjs';
import './ui.test.mjs';
import './grouping.test.mjs';
import './uploads.test.mjs';
import './transcribe.test.mjs';
import './files.test.mjs';
import './touched.test.mjs';
import './briefs.test.mjs';
import './notes.test.mjs';
import './spawn-dirs.test.mjs';
import './dormant.test.mjs';
import './repos.test.mjs';
import './jobs.test.mjs';
import './memos.test.mjs';
import './diff.test.mjs';
import './reviews.test.mjs';
import './focus.test.mjs';
import './inbox.test.mjs';
import './slack.test.mjs';
