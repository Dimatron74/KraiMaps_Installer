<script setup lang="ts">
import { onMounted } from 'vue';
import { useRouter } from 'vue-router';
import { useInstallStore } from '../../../app/stores/install';
import { useSiteInstallStore } from '../../../app/stores/siteInstall';
import Alert from '../../../shared/ui/Alert.vue';
import Btn from '../../../shared/ui/Btn.vue';

const store = useInstallStore();
const site = useSiteInstallStore();
const router = useRouter();

onMounted(() => {
  if (site.idempotencyKey) {
    void site.reportCompleted(store.target?.launcher ?? null);
  }
});

function goHome() {
  store.reset();
  site.reset();
  router.push('/');
}
</script>

<template>
  <div class="flex flex-col items-center py-8 text-center">
    <div class="flex h-16 w-16 items-center justify-center rounded-full bg-success-500/15 border border-success-500/30">
      <svg class="h-8 w-8 text-success-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true">
        <path stroke-linecap="round" stroke-linejoin="round" d="M5 13l4 4L19 7" />
      </svg>
    </div>
    <h2 class="mt-4 text-xl font-bold text-primary">Готово!</h2>
    <p class="mt-2 max-w-md text-sm text-secondary">
      Мир «{{ store.report?.world_dir_name }}» установлен
      <template v-if="store.target">в «{{ store.target.name }}»</template><template v-if="store.report && store.report.mods_written.length > 0">
        вместе с {{ store.report.mods_written.length }} модами</template><template v-if="store.report && store.report.resource_packs_written.length > 0">
        и {{ store.report.resource_packs_written.length }} ресурспаками</template>.
    </p>

    <Alert v-if="store.report && store.report.backed_up.length > 0" tone="info" class="mt-4 max-w-md text-left">
      <p class="text-xs">
        Заменённые файлы сохранены в
        <span class="font-mono">.kraimaps-backup/</span> ({{ store.report.backed_up.length }}).
      </p>
    </Alert>

    <Alert v-if="store.tlauncherNote" tone="warning" class="mt-4 max-w-md text-left">
      <p class="text-xs">{{ store.tlauncherNote }}</p>
    </Alert>

    <div class="mt-6 flex w-full max-w-md flex-col gap-2">
      <Btn
        v-if="store.target?.launcher === 'manual'"
        variant="success"
        size="lg"
        :loading="store.busy"
        @click="store.play()"
      >
        Открыть папку с миром
      </Btn>
      <Btn v-else variant="success" size="lg" :loading="store.busy" @click="store.play()">
        Играть
      </Btn>
      <Btn variant="secondary" @click="goHome()">На главную</Btn>
    </div>
  </div>
</template>
