<script setup lang="ts">
import { open } from '@tauri-apps/plugin-dialog';
import { useInstallStore } from '../../../app/stores/install';
import Btn from '../../../shared/ui/Btn.vue';

const store = useInstallStore();

async function pick() {
  const selected = await open({
    multiple: false,
    filters: [{ name: 'Карта Minecraft', extensions: ['zip'] }],
  });
  if (typeof selected === 'string') {
    store.loadRoots();
    await store.pickZip(selected);
  }
}
</script>

<template>
  <!-- Анализ идёт: показываем процесс, а не тишину -->
  <div v-if="store.step === 'analyzing'" class="flex flex-col items-center py-10 text-center">
    <svg
      class="animate-spin h-10 w-10 text-primary-400"
      viewBox="0 0 24 24"
      fill="none"
      aria-hidden="true"
    >
      <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4" />
      <path
        class="opacity-75"
        fill="currentColor"
        d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z"
      />
    </svg>
    <h2 class="mt-5 text-lg font-bold text-primary">Анализируем архив…</h2>
    <p class="mt-2 max-w-md truncate text-sm text-secondary">{{ store.zipPath }}</p>
    <p class="mt-1 text-xs text-tertiary">Ищем мир, определяем версию и моды. Большие архивы могут занять несколько десятков секунд.</p>
  </div>

  <div v-else class="flex flex-col items-center py-10 text-center">
    <div class="flex h-16 w-16 items-center justify-center rounded-2xl bg-primary-500/10 border border-primary-500/20">
      <svg class="text-primary-400" xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
        <circle cx="15" cy="19" r="2"/>
        <path d="M20.9 19.8A2 2 0 0 0 22 18V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2h5.1"/>
        <path d="M15 11v-1"/>
        <path d="M15 17v-2"/>
      </svg>
    </div>
    <h2 class="mt-5 text-xl font-bold text-primary">Установка карты</h2>
    <p class="mt-2 max-w-md text-sm text-secondary">
      Выберите zip-архив карты. Установщик разберёт его, подберёт подходящий инстанс
      и поставит мир, моды и настройки одной кнопкой.
    </p>
    <Btn class="mt-6" size="lg" @click="pick">Выбрать zip-архив</Btn>
  </div>
</template>
