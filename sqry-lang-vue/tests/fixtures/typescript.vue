<template>
  <div>
    <h1>{{ message }}</h1>
  </div>
</template>

<script lang="ts">
interface User {
  name: string;
  age: number;
}

function formatUser(user: User): string {
  return `${user.name} (${user.age})`;
}

export default {
  data() {
    return {
      message: getMessage()
    };
  }
};

function getMessage(): string {
  const user: User = { name: 'Alice', age: 30 };
  return formatUser(user);
}
</script>
