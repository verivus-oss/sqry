<!-- Hand-written Svelte component fixture for the body-shape coverage test. The
     control flow lives in the embedded <script> block (parsed with the
     JavaScript grammar), so the shape descriptor for `classify` is computed
     against the JS subtree, not the Svelte markup tree. A top-level function
     declaration records a real body span, which is what makes the node
     eligible for a descriptor. -->
<script>
  function classify(score) {
    let label = "";
    if (score >= 90) {
      label = "A";
    } else {
      label = "B";
    }
    for (let i = 0; i < score; i++) {
      record(i);
    }
    return label;
  }
</script>

<div class="card" on:click={() => classify(5)}>{label}</div>
