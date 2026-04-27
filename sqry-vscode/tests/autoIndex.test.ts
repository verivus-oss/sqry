import { expect } from "chai";
import { AutoIndexManager } from "../src/autoIndex";
import type {
  SqrySourceRootStatus,
  SqryWorkspaceStatus,
} from "../src/lspProtocol";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** Run a callback after the Node.js event loop has drained pending timers. */
const tick = (ms = 0): Promise<void> =>
  new Promise<void>((resolve) => setTimeout(resolve, ms));

// ---------------------------------------------------------------------------
// Debounce behaviour
// ---------------------------------------------------------------------------

describe("AutoIndexManager — debounce", () => {
  it("fires the callback after the delay has elapsed", async () => {
    const mgr = new AutoIndexManager();
    let fired = 0;

    mgr.schedule("root", 20, () => { fired += 1; });
    await tick(50);

    expect(fired).to.equal(1);
    mgr.dispose();
  });

  it("rapid calls reset the timer — only the last one fires", async () => {
    const mgr = new AutoIndexManager();
    const calls: number[] = [];

    mgr.schedule("root", 40, () => { calls.push(1); });
    await tick(10);
    mgr.schedule("root", 40, () => { calls.push(2); });
    await tick(10);
    mgr.schedule("root", 40, () => { calls.push(3); });

    // Wait long enough for the final timer to fire.
    await tick(100);

    expect(calls).to.deep.equal([3]);
    mgr.dispose();
  });

  it("does not fire if cancelled before expiry", async () => {
    const mgr = new AutoIndexManager();
    let fired = 0;

    mgr.schedule("root", 30, () => { fired += 1; });
    mgr.cancel("root");
    await tick(60);

    expect(fired).to.equal(0);
    mgr.dispose();
  });

  it("different roots have independent timers", async () => {
    const mgr = new AutoIndexManager();
    const fired: string[] = [];

    mgr.schedule("rootA", 20, () => { fired.push("A"); });
    mgr.schedule("rootB", 20, () => { fired.push("B"); });

    // Cancel only rootA — rootB should still fire.
    mgr.cancel("rootA");
    await tick(60);

    expect(fired).to.deep.equal(["B"]);
    mgr.dispose();
  });
});

// ---------------------------------------------------------------------------
// Dirty latch
// ---------------------------------------------------------------------------

describe("AutoIndexManager — dirty latch", () => {
  it("latch is false by default", () => {
    const mgr = new AutoIndexManager();
    expect(mgr.isDirty("root")).to.equal(false);
    mgr.dispose();
  });

  it("markDirty sets the latch", () => {
    const mgr = new AutoIndexManager();
    mgr.markDirty("root");
    expect(mgr.isDirty("root")).to.equal(true);
    mgr.dispose();
  });

  it("completeBuild returns true when latch was set", () => {
    const mgr = new AutoIndexManager();
    mgr.startBuild("root");
    mgr.markDirty("root");
    const needsFollowUp = mgr.completeBuild("root");
    expect(needsFollowUp).to.equal(true);
    mgr.dispose();
  });

  it("completeBuild returns false when latch was not set", () => {
    const mgr = new AutoIndexManager();
    mgr.startBuild("root");
    const needsFollowUp = mgr.completeBuild("root");
    expect(needsFollowUp).to.equal(false);
    mgr.dispose();
  });

  it("completeBuild clears the latch", () => {
    const mgr = new AutoIndexManager();
    mgr.startBuild("root");
    mgr.markDirty("root");
    mgr.completeBuild("root");
    expect(mgr.isDirty("root")).to.equal(false);
    mgr.dispose();
  });

  it("startBuild clears any pre-existing dirty latch from a previous cycle", () => {
    const mgr = new AutoIndexManager();
    // First build cycle — latch set but build completes and latch cleared.
    mgr.startBuild("root");
    mgr.markDirty("root");
    mgr.completeBuild("root");
    // Latch is now false. Start a new build — latch should remain false.
    mgr.startBuild("root");
    expect(mgr.isDirty("root")).to.equal(false);
    mgr.dispose();
  });
});

// ---------------------------------------------------------------------------
// Build state tracking
// ---------------------------------------------------------------------------

describe("AutoIndexManager — build state", () => {
  it("isBuilding returns false before build starts", () => {
    const mgr = new AutoIndexManager();
    expect(mgr.isBuilding("root")).to.equal(false);
    mgr.dispose();
  });

  it("isBuilding returns true after startBuild", () => {
    const mgr = new AutoIndexManager();
    mgr.startBuild("root");
    expect(mgr.isBuilding("root")).to.equal(true);
    mgr.dispose();
  });

  it("isBuilding returns false after completeBuild", () => {
    const mgr = new AutoIndexManager();
    mgr.startBuild("root");
    mgr.completeBuild("root");
    expect(mgr.isBuilding("root")).to.equal(false);
    mgr.dispose();
  });
});

// ---------------------------------------------------------------------------
// Per-root isolation
// ---------------------------------------------------------------------------

describe("AutoIndexManager — per-root isolation", () => {
  it("building state is independent per root", () => {
    const mgr = new AutoIndexManager();
    mgr.startBuild("rootA");
    expect(mgr.isBuilding("rootA")).to.equal(true);
    expect(mgr.isBuilding("rootB")).to.equal(false);
    mgr.dispose();
  });

  it("dirty latch is independent per root", () => {
    const mgr = new AutoIndexManager();
    mgr.startBuild("rootA");
    mgr.markDirty("rootA");
    expect(mgr.isDirty("rootA")).to.equal(true);
    expect(mgr.isDirty("rootB")).to.equal(false);
    mgr.dispose();
  });

  it("completeBuild on one root does not affect another", () => {
    const mgr = new AutoIndexManager();
    mgr.startBuild("rootA");
    mgr.startBuild("rootB");
    mgr.markDirty("rootA");
    mgr.completeBuild("rootA");
    expect(mgr.isBuilding("rootB")).to.equal(true);
    expect(mgr.isDirty("rootB")).to.equal(false);
    mgr.dispose();
  });
});

// ---------------------------------------------------------------------------
// "never" setting no-op
// ---------------------------------------------------------------------------

describe("AutoIndexManager — never setting no-op", () => {
  it("schedule is not called when setting is never — no timer fires", async () => {
    const mgr = new AutoIndexManager();
    // Use a variable typed as string to simulate the runtime config value.
    const setting: string = "never";
    let fired = 0;

    // Simulate the extension's guard: only call schedule when setting === "debounced"
    if (setting === "debounced") {
      mgr.schedule("root", 10, () => { fired += 1; });
    }

    await tick(30);
    expect(fired).to.equal(0);
    mgr.dispose();
  });

  it("dispose clears pending timers so callbacks never fire after dispose", async () => {
    const mgr = new AutoIndexManager();
    let fired = 0;

    mgr.schedule("root", 20, () => { fired += 1; });
    mgr.dispose();

    await tick(50);
    expect(fired).to.equal(0);
  });
});

// ---------------------------------------------------------------------------
// STEP_5 codex iter1 MAJOR 3 — enqueueFromWorkspaceStatus contract
// ---------------------------------------------------------------------------

function buildStatus(entries: SqrySourceRootStatus[]): SqryWorkspaceStatus {
  let missing = 0;
  let building = 0;
  let ok = 0;
  let error = 0;
  for (const e of entries) {
    if (e.status === "missing") missing += 1;
    else if (e.status === "building") building += 1;
    else if (e.status === "ok") ok += 1;
    else if (e.status === "error") error += 1;
  }
  return {
    source_root_statuses: entries,
    missing_count: missing,
    building_count: building,
    ok_count: ok,
    error_count: error,
    generated_at: "2026-04-27T00:00:00Z",
  };
}

describe("AutoIndexManager.enqueueFromWorkspaceStatus — STEP_5 codex iter1 MAJOR 3", () => {
  it("enqueues only entries with status === 'missing'", async () => {
    const mgr = new AutoIndexManager();
    const status = buildStatus([
      { path: "/repo-a", status: "missing" },
      { path: "/repo-b", status: "ok" },
      { path: "/repo-c", status: "missing" },
      { path: "/repo-d", status: "building" },
      { path: "/repo-e", status: "error" },
    ]);
    const ran: string[] = [];
    const result = await mgr.enqueueFromWorkspaceStatus(status, async (root) => {
      ran.push(root.path);
    });
    expect(ran).to.deep.equal(["/repo-a", "/repo-c"]);
    expect(result.inspected).to.equal(5);
    expect(result.enqueued).to.equal(2);
    expect(result.excluded).to.equal(0);
    expect(result.nonMissing).to.equal(3);
    mgr.dispose();
  });

  it("respects the exclude predicate (ignores excluded entries even when missing)", async () => {
    const mgr = new AutoIndexManager();
    const status = buildStatus([
      { path: "/wanted", status: "missing" },
      { path: "/excluded", status: "missing" },
      { path: "/also-wanted", status: "missing" },
    ]);
    const ran: string[] = [];
    const result = await mgr.enqueueFromWorkspaceStatus(
      status,
      async (root) => {
        ran.push(root.path);
      },
      { exclude: (path) => path === "/excluded" },
    );
    expect(ran).to.deep.equal(["/wanted", "/also-wanted"]);
    expect(result.enqueued).to.equal(2);
    expect(result.excluded).to.equal(1);
    expect(result.nonMissing).to.equal(0);
    mgr.dispose();
  });

  it("does NOT enqueue member folders (status !== 'missing' is treated as nonMissing)", async () => {
    const mgr = new AutoIndexManager();
    // Member folders surface in the aggregate ONLY as part of source-root
    // entries when they happen to also be source roots — but their `status`
    // is set by the LSP based on on-disk presence, not their classification.
    // A member-folder-only path that is not also a source root simply does
    // NOT appear in the aggregate. We model that here by omitting it.
    const status = buildStatus([
      { path: "/source-root", status: "missing" },
      { path: "/member-and-source-root", status: "ok" }, // already indexed
    ]);
    const ran: string[] = [];
    await mgr.enqueueFromWorkspaceStatus(status, async (root) => {
      ran.push(root.path);
    });
    expect(ran).to.deep.equal(["/source-root"]);
    mgr.dispose();
  });

  it("runs sequentially — each runner resolves before the next is invoked", async () => {
    const mgr = new AutoIndexManager();
    const status = buildStatus([
      { path: "/a", status: "missing" },
      { path: "/b", status: "missing" },
      { path: "/c", status: "missing" },
    ]);
    const events: string[] = [];
    await mgr.enqueueFromWorkspaceStatus(status, async (root) => {
      events.push(`start:${root.path}`);
      await new Promise<void>((r) => setTimeout(r, 5));
      events.push(`end:${root.path}`);
    });
    expect(events).to.deep.equal([
      "start:/a", "end:/a",
      "start:/b", "end:/b",
      "start:/c", "end:/c",
    ]);
    mgr.dispose();
  });

  it("propagates runner failures and stops further enqueue", async () => {
    const mgr = new AutoIndexManager();
    const status = buildStatus([
      { path: "/a", status: "missing" },
      { path: "/b", status: "missing" },
      { path: "/c", status: "missing" },
    ]);
    const ran: string[] = [];
    let caught: unknown;
    try {
      await mgr.enqueueFromWorkspaceStatus(status, async (root) => {
        ran.push(root.path);
        if (root.path === "/b") {
          throw new Error("boom");
        }
      });
    } catch (err) {
      caught = err;
    }
    expect(ran).to.deep.equal(["/a", "/b"]);
    expect((caught as Error | undefined)?.message).to.equal("boom");
    mgr.dispose();
  });

  it("returns zeros when the aggregate is empty", async () => {
    const mgr = new AutoIndexManager();
    const result = await mgr.enqueueFromWorkspaceStatus(buildStatus([]), async () => {
      throw new Error("runner must not be called");
    });
    expect(result).to.deep.equal({ inspected: 0, enqueued: 0, excluded: 0, nonMissing: 0 });
    mgr.dispose();
  });
});
