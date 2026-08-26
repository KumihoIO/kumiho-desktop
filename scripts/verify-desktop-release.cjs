const SEMVER = /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-((?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*))*))?(?:\+([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$/;
const SHA = /^(?:[0-9a-f]{40}|[0-9a-f]{64})$/i;

function parseReleaseTag(tag) {
  if (!String(tag || '').startsWith('desktop-v')) {
    throw new Error('Release tag must start with desktop-v: ' + String(tag || ''));
  }
  const version = tag.slice('desktop-v'.length);
  if (!SEMVER.test(version)) {
    throw new Error('Release tag is not valid SemVer: ' + tag);
  }
  return version;
}

function normalizeSha(value, label) {
  const sha = String(value || '').toLowerCase();
  if (!SHA.test(sha)) throw new Error(label + ' is not a full Git commit SHA');
  return sha;
}

function verifyReleaseInputs(input) {
  if (input.refType !== 'tag') {
    throw new Error('Desktop releases must run from an existing tag, not a branch');
  }
  const version = parseReleaseTag(input.tag);
  if (input.tagVersion !== undefined && input.tagVersion !== version) {
    throw new Error('Release tag version input does not match ' + input.tag);
  }
  const expectedSha = normalizeSha(input.expectedSha, 'Workflow SHA');
  const localTagCommit = normalizeSha(input.localTagCommit, 'Local peeled tag commit');
  if (localTagCommit !== expectedSha) {
    throw new Error(
      `Local tag ${input.tag} resolves to ${localTagCommit}, which does not match workflow SHA ${expectedSha}`,
    );
  }
  if (version !== input.cargoVersion || version !== input.tauriVersion) {
    throw new Error(
      `Release tag version ${version} does not match Cargo ${input.cargoVersion} and Tauri ${input.tauriVersion}`,
    );
  }
  return { tag: input.tag, version, expectedSha };
}

function responseObject(response, label) {
  const object = response && response.data && response.data.object;
  if (!object || !object.type || !object.sha) {
    throw new Error(label + ' did not return a Git object');
  }
  return object;
}

async function resolveRemoteTagCommit({ github, owner, repo, tag, initialRef }) {
  const ref = initialRef || await github.rest.git.getRef({ owner, repo, ref: `tags/${tag}` });
  let object = responseObject(ref, `Remote tag ${tag}`);
  const seen = new Set();

  for (let depth = 0; depth < 16; depth += 1) {
    const sha = normalizeSha(object.sha, `Remote ${object.type} object SHA`);
    if (object.type === 'commit') return sha;
    if (object.type !== 'tag') {
      throw new Error(`Remote tag ${tag} resolves to unsupported Git object type ${object.type}`);
    }
    if (seen.has(sha)) throw new Error(`Remote tag ${tag} contains an annotated-tag cycle`);
    seen.add(sha);
    const annotated = await github.rest.git.getTag({ owner, repo, tag_sha: sha });
    object = responseObject(annotated, `Annotated tag object ${sha}`);
  }
  throw new Error(`Remote tag ${tag} exceeds the annotated-tag depth limit`);
}

async function assertRemoteTagCommit(options) {
  const expectedSha = normalizeSha(options.expectedSha, 'Workflow SHA');
  const actualSha = await resolveRemoteTagCommit(options);
  if (actualSha !== expectedSha) {
    throw new Error(
      `Remote tag ${options.tag} resolves to ${actualSha}, which does not match workflow SHA ${expectedSha}`,
    );
  }
  return actualSha;
}

function releaseCreateParams({ owner, repo, tag, expectedSha }) {
  return {
    owner,
    repo,
    tag_name: tag,
    target_commitish: normalizeSha(expectedSha, 'Workflow SHA'),
    name: `Kumiho Desktop ${tag}`,
    draft: true,
    prerelease: false,
  };
}

async function ensureDraftRelease({ github, owner, repo, tag, expectedSha, initialRef }) {
  parseReleaseTag(tag);
  const expected = normalizeSha(expectedSha, 'Workflow SHA');
  await assertRemoteTagCommit({ github, owner, repo, tag, expectedSha: expected, initialRef });

  const all = await github.paginate(github.rest.repos.listReleases, { owner, repo });
  const found = all.find((release) => release.tag_name === tag);
  let releaseId;
  if (found) {
    releaseId = found.id;
  } else {
    const { data } = await github.rest.repos.createRelease(
      releaseCreateParams({ owner, repo, tag, expectedSha: expected }),
    );
    releaseId = data.id;
  }

  // A tag can be moved or deleted while a run is queued. Re-check after the
  // create/reuse decision so no release is accepted for a different commit.
  await assertRemoteTagCommit({ github, owner, repo, tag, expectedSha: expected });
  return releaseId;
}

function runCli() {
  try {
    const verified = verifyReleaseInputs({
      refType: process.env.GITHUB_REF_TYPE,
      tag: process.env.GITHUB_REF_NAME,
      tagVersion: process.env.TAG_VERSION,
      expectedSha: process.env.GITHUB_SHA,
      localTagCommit: process.env.LOCAL_TAG_COMMIT,
      cargoVersion: process.env.CARGO_VERSION,
      tauriVersion: process.env.TAURI_VERSION,
    });
    console.log(`Verified ${verified.tag} at ${verified.expectedSha}`);
  } catch (error) {
    console.error('::error::' + error.message);
    process.exitCode = 1;
  }
}

module.exports = {
  assertRemoteTagCommit,
  ensureDraftRelease,
  parseReleaseTag,
  releaseCreateParams,
  resolveRemoteTagCommit,
  verifyReleaseInputs,
};

if (require.main === module) runCli();
