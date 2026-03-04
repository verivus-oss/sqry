<template>
  <div>
    <h1>{{ title }}</h1>
  </div>
</template>

<script>
// Regular script for component options
export default {
  name: 'MixedComponent',
  data() {
    return {
      title: getTitle()
    };
  }
};

function getTitle() {
  return formatTitle('Hello Vue');
}

function formatTitle(text) {
  return text.toUpperCase();
}
</script>

<script setup>
// Setup script for Composition API
import { onMounted } from 'vue';

function initializeComponent() {
  setupLogger();
}

function setupLogger() {
  console.log('Component initialized');
}

onMounted(() => {
  initializeComponent();
});
</script>
