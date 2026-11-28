/* A worker outlives the initial pthread and finishes when its caller releases it. */
#define _GNU_SOURCE
#include <pthread.h>
#include <stdio.h>
#include <unistd.h>

static void *work(void *unused) {
    FILE *ids = fopen("worker-ids.tmp", "w");
    if (!ids) return NULL;
    fprintf(ids, "%d %d", getpid(), gettid());
    fclose(ids);
    rename("worker-ids.tmp", "worker-ids");
    for (int i = 0; i < 2000 && access("go", F_OK) != 0; ++i) usleep(10000);
    puts("worker finished");
    return NULL;
}

int main(void) {
    pthread_t worker;
    if (pthread_create(&worker, NULL, work, NULL) != 0) return 1;
    pthread_exit(NULL);
}
