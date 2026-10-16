<script setup lang="ts">
/**
 * Скелетон-заглушка (пульсирующий блок).
 * Размеры — через props инлайн-стилем, чтобы не бороться с Tailwind-purge.
 */
defineProps<{
  width?: string;
  height?: string;
  rounded?: string;
}>();
</script>

<template>
  <div
    class="animate-pulse bg-white/[0.06]"
    aria-hidden="true"
    :style="{
      width: width ?? '100%',
      height: height ?? '1rem',
      borderRadius: rounded ?? '0.75rem',
    }"
  />
</template>
