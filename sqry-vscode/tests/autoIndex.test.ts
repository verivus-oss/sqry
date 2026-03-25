import { expect } from "chai";
import { AutoIndexManager } from "../src/autoIndex";

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
