const assert = require('node:assert/strict');

const {
  ensureDraftRelease,
  resolveRemoteTagCommit,
  verifyReleaseInputs,
} = require('./verify-desktop-release.cjs');

const EXPECTED_SHA = 'a'.repeat(40);
const MOVED_SHA = 'b'.repeat(40);

function validInputs(overrides = {}) {
  return {
    refType: 'tag',
    tag: 'desktop-v0.4.4',
    tagVersion: '0.4.4',
    expectedSha: EXPECTED_SHA,
    localTagCommit: EXPECTED_SHA,
    cargoVersion: '0.4.4',
    tauriVersion: '0.4.4',
    ...overrides,
  };
}

function refResponse(type, sha) {
  return { data: { object: { type, sha } } };
}

assert.equal(verifyReleaseInputs(validInputs()).version, '0.4.4');
assert.throws(
  () => verifyReleaseInputs(validInputs({ refType: 'branch', tag: 'main' })),
  /existing tag/i,
);
assert.throws(
  () => verifyReleaseInputs(validInputs({ tag: 'desktop-v01.4.4', tagVersion: '01.4.4' })),
  /valid SemVer/i,
);
assert.throws(
  () => verifyReleaseInputs(validInputs({ cargoVersion: '0.4.3' })),
  /does not match Cargo 0\.4\.3 and Tauri 0\.4\.4/i,
);
assert.throws(
  () => verifyReleaseInputs(validInputs({ localTagCommit: MOVED_SHA })),
  /local tag .* does not match workflow SHA/i,
);

async function main() {
  {
    const tagObjects = new Map([
      ['1'.repeat(40), { type: 'tag', sha: '2'.repeat(40) }],
      ['2'.repeat(40), { type: 'commit', sha: EXPECTED_SHA }],
    ]);
    const github = {
      rest: {
        git: {
          getRef: async () => refResponse('tag', '1'.repeat(40)),
          getTag: async ({ tag_sha: tagSha }) => ({ data: { object: tagObjects.get(tagSha) } }),
        },
      },
    };
    assert.equal(
      await resolveRemoteTagCommit({ github, owner: 'KumihoIO', repo: 'kumiho-desktop', tag: 'desktop-v0.4.4' }),
      EXPECTED_SHA,
    );
  }

  {
    let createdWith;
    let postChecks = 0;
    const github = {
      paginate: async () => [],
      rest: {
        git: {
          getRef: async () => {
            postChecks += 1;
            return refResponse('commit', EXPECTED_SHA);
          },
          getTag: async () => { throw new Error('lightweight tag must not be peeled'); },
        },
        repos: {
          listReleases: async () => {},
          createRelease: async (params) => {
            createdWith = params;
            return { data: { id: 44 } };
          },
        },
      },
    };
    const releaseId = await ensureDraftRelease({
      github,
      owner: 'KumihoIO',
      repo: 'kumiho-desktop',
      tag: 'desktop-v0.4.4',
      expectedSha: EXPECTED_SHA,
      initialRef: refResponse('commit', EXPECTED_SHA),
    });
    assert.equal(releaseId, 44);
    assert.equal(createdWith.target_commitish, EXPECTED_SHA);
    assert.equal(createdWith.tag_name, 'desktop-v0.4.4');
    assert.equal(postChecks, 1);
  }

  {
    let releaseLookupRan = false;
    const github = {
      paginate: async () => { releaseLookupRan = true; return []; },
      rest: {
        git: {
          getTag: async () => { throw new Error('lightweight tag must not be peeled'); },
        },
        repos: {},
      },
    };
    await assert.rejects(
      ensureDraftRelease({
        github,
        owner: 'KumihoIO',
        repo: 'kumiho-desktop',
        tag: 'desktop-v0.4.4',
        expectedSha: EXPECTED_SHA,
        initialRef: refResponse('commit', MOVED_SHA),
      }),
      /remote tag .* does not match workflow SHA/i,
    );
    assert.equal(releaseLookupRan, false);
  }

  {
    let getRefCalls = 0;
    const github = {
      paginate: async () => [{ id: 45, tag_name: 'desktop-v0.4.4' }],
      rest: {
        git: {
          getRef: async () => {
            getRefCalls += 1;
            return refResponse('commit', MOVED_SHA);
          },
          getTag: async () => { throw new Error('lightweight tag must not be peeled'); },
        },
        repos: { listReleases: async () => {} },
      },
    };
    await assert.rejects(
      ensureDraftRelease({
        github,
        owner: 'KumihoIO',
        repo: 'kumiho-desktop',
        tag: 'desktop-v0.4.4',
        expectedSha: EXPECTED_SHA,
        initialRef: refResponse('commit', EXPECTED_SHA),
      }),
      /remote tag .* does not match workflow SHA/i,
    );
    assert.equal(getRefCalls, 1);
  }

  console.log('Desktop release gate checks passed');
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
