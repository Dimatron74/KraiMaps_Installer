<script setup lang="ts">
import { computed, onMounted } from 'vue';
import { useRouter } from 'vue-router';
import { useInstallStore } from '../../../app/stores/install';
import { useSiteInstallStore } from '../../../app/stores/siteInstall';
import Alert from '../../../shared/ui/Alert.vue';
import Btn from '../../../shared/ui/Btn.vue';
import CompatDetails from '../components/CompatDetails.vue';

const store = useInstallStore();
const site = useSiteInstallStore();
const router = useRouter();

/** Отчёт слоя: `null` — слой не пытались ставить (существующая цель). */
const compat = computed(() => store.report?.compat ?? null);
const compatFailed = computed(() => !!compat.value?.error);

/** Сеть/stall у OptiFine в России часто лечится VPN — говорим прямо. */
const compatVpnHint = computed(() => {
  const err = compat.value?.error;
  if (!err || compat.value?.kind !== 'optifine') return null;
  return ['E_COMPAT_STALLED', 'E_COMPAT_DOWNLOAD', 'E_COMPAT_RESOLVE', 'E_COMPAT_UNAVAILABLE'].includes(
    err.code,
  )
    ? 'Если optifine.net не отвечает, в России может понадобиться VPN.'
    : null;
});

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

    <Alert v-if="compat && !compatFailed" tone="success" class="mt-4 max-w-md text-left">
      <p class="text-xs">
        Сборка совместимости установлена: {{ compat.display_name }}.
      </p>
      <CompatDetails :summary="store.compat" :report="compat" />
    </Alert>

    <Alert v-else-if="store.compatUnavailable" tone="warning" class="mt-4 max-w-md text-left">
      <p class="text-xs">{{ store.compatUnavailable }}</p>
      <p v-if="store.compatPreparationFailure" class="mt-1 text-xs">
        Карта установлена без OptiFine. Проверьте подключение; новую попытку со сборкой можно
        начать с главной.
      </p>
      <CompatDetails :summary="store.compat" :failure="store.compatPreparationFailure" />
    </Alert>

    <Alert
      v-else-if="store.compatSkipReason && !compatFailed"
      :tone="store.targetIsFresh ? 'warning' : 'info'"
      class="mt-4 max-w-md text-left"
    >
      <p class="text-xs">{{ store.compatSkipReason }}</p>
      <CompatDetails :summary="store.compat" />
    </Alert>

    <Alert v-else-if="compatFailed" tone="warning" class="mt-4 max-w-md text-left">
      <p class="text-xs">
        Карта установлена, но сборка совместимости не встала: {{ compat?.error?.message }}.
      </p>
      <p class="mt-1 text-xs">Закройте игру, проверьте подключение и нажмите «Повторить».</p>
      <p v-if="compatVpnHint" class="mt-1 text-xs">{{ compatVpnHint }}</p>
      <CompatDetails :summary="store.compat" :report="compat" />
      <Btn
        v-if="store.compatInDeploy"
        variant="secondary"
        class="mt-3"
        :loading="store.busy"
        @click="store.retryCompat()"
      >
        Повторить
      </Btn>
    </Alert>

    <Alert v-if="store.report && store.report.backed_up.length > 0" tone="info" class="mt-4 max-w-md text-left">
      <p class="text-xs">
        Заменённые файлы сохранены в
        <span class="font-mono">.kraimaps-backup/</span> ({{ store.report.backed_up.length }}).
      </p>
    </Alert>

    <Alert v-if="store.versionNote" tone="warning" class="mt-4 max-w-md text-left">
      <p class="text-xs">{{ store.versionNote }}</p>
    </Alert>

    <div class="mt-6 flex w-full max-w-md flex-col gap-2">
      <template v-if="store.launcher?.app_found">
        <Btn variant="success" size="lg" :loading="store.busy" @click="store.play()">
          Играть
        </Btn>
      </template>
      <Alert v-else tone="info">
        <p class="text-xs">
          Лаунчер не найден — осталась только папка с данными. Карта установлена, но запустить её отсюда
          не получится.
        </p>
      </Alert>
      <Btn variant="secondary" :disabled="store.busy" @click="goHome()">На главную</Btn>
    </div>
  </div>
</template>
