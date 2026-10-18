<script setup lang="ts">
import { onMounted } from 'vue';
import { useInstallStore } from '../../../app/stores/install';
import { useSiteInstallStore } from '../../../app/stores/siteInstall';
import Btn from '../../../shared/ui/Btn.vue';
import ErrorDetails from '../../../shared/ui/ErrorDetails.vue';

const store = useInstallStore();
const site = useSiteInstallStore();

onMounted(() => {
  if (site.idempotencyKey && store.error) {
    void site.reportFailed(store.error.code);
  }
});

function back() {
  store.error = null;
  store.step = store.errorReturnStep;
}
</script>

<template>
  <div>
    <h2 class="text-lg font-bold text-primary">Не получилось</h2>
    <ErrorDetails
      v-if="store.error"
      class="mt-3"
      :code="store.error.code"
      :message="store.error.message"
      :details="store.error.details"
      :hint="store.error.hint"
      :action="store.error.action"
    />
    <div class="mt-5 flex gap-2">
      <Btn variant="secondary" @click="back()">Назад</Btn>
      <Btn variant="secondary" @click="store.reset()">Сначала</Btn>
    </div>
  </div>
</template>
