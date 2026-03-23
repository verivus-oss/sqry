<template>
  <AppLayout>
    <template #header>
      <HeaderBar @refresh="handleRefresh" />
    </template>

    <section>
      <TodoList
        :items="filteredTodos"
        @select="handleSelect"
      />

      <FancyButton @click="handleClick">
        {{ buttonLabel }}
      </FancyButton>

      <input
        v-model="searchTerm"
        placeholder="Search items"
      />

      <component :is="currentComponent" />
    </section>
  </AppLayout>
</template>

<script setup>
import { computed, ref } from 'vue'

import AppLayout from './AppLayout.vue'
import FancyButton from './FancyButton.vue'
import HeaderBar from './HeaderBar.vue'
import TodoList from './TodoList.vue'

const props = defineProps({
  todos: {
    type: Array,
    default: () => [],
  },
})

const emit = defineEmits(['update:searchTerm', 'refresh', 'select'])

const searchTerm = ref('')
const buttonLabel = ref('Apply Filter')
const currentComponent = ref('TodoList')

const filteredTodos = computed(() =>
  props.todos.filter((todo) =>
    todo.title
      .toLowerCase()
      .includes(searchTerm.value.toLowerCase()),
  ),
)

function handleClick() {
  emit('update:searchTerm', searchTerm.value)
}

function handleRefresh() {
  emit('refresh')
}

function handleSelect(item) {
  currentComponent.value = item.component
  emit('select', item)
}
</script>
