<script setup lang="ts">
/** Технический экран: обмен code→tokens делает Rust по deep-link; это запасной путь. */
import { onMounted, ref } from 'vue';
import { useRouter } from 'vue-router';
import { useAuthStore } from '../../app/stores/auth';
import { useSiteInstallStore } from '../../app/stores/siteInstall';

const auth = useAuthStore();
const site = useSiteInstallStore();
const router = useRouter();
const message = ref('Завершаем вход…');

onMounted(async () => {
  try {
    await auth.refresh();
  } finally {
    if (auth.isAuthed && auth.pendingMapId !== null) {
      const id = auth.pendingMapId;
      const mode = auth.pendingMode;
      auth.pendingMapId = null;
      message.value = 'Вход выполнен. Загружаем карту…';
      router.replace('/install');
      await site.startFromSite(id, mode, 'Вход выполнен');
    } else if (auth.isAuthed) {
      router.replace('/');
    } else {
      router.replace({ name: 'enter', query: auth.pendingMapId ? { mapId: String(auth.pendingMapId) } : {} });
    }
  }
});
</script>

<template>
  <div class="flex flex-col items-center py-10 text-center">
    <svg class="h-10 w-10 animate-spin text-primary-400" viewBox="0 0 24 24" fill="none" aria-hidden="true">
      <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4" />
      <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z" />
    </svg>
    <p class="mt-4 text-sm text-secondary">{{ message }}</p>
  </div>
</template>
