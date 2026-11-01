// Sums macOS phys_footprint (Activity Monitor's "Memory") over a process and all its descendants.
// Usage: footprint <root-pid> [interval-ms]  -> prints "<unix-ms>,<bytes>,<nprocs>" until the root exits.
#include <libproc.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/time.h>
#include <unistd.h>

static void walk(pid_t pid, pid_t self, unsigned long long *bytes, int *n) {
  struct rusage_info_v4 ri;
  if (pid != self && proc_pid_rusage(pid, RUSAGE_INFO_V4, (rusage_info_t *)&ri) == 0) {
    *bytes += ri.ri_phys_footprint;
    (*n)++;
  }
  pid_t kids[4096];
  int k = proc_listchildpids(pid, kids, sizeof kids) ;
  for (int i = 0; i < k && i < 4096; i++) walk(kids[i], self, bytes, n);
}

int main(int argc, char **argv) {
  if (argc < 2) { fprintf(stderr, "usage: footprint <pid> [interval-ms]\n"); return 2; }
  pid_t root = atoi(argv[1]);
  int ms = argc > 2 ? atoi(argv[2]) : 250;
  setvbuf(stdout, NULL, _IOLBF, 0);
  while (kill(root, 0) == 0) {
    unsigned long long bytes = 0; int n = 0;
    walk(root, getpid(), &bytes, &n);
    struct timeval tv; gettimeofday(&tv, NULL);
    printf("%lld,%llu,%d\n", (long long)tv.tv_sec * 1000 + tv.tv_usec / 1000, bytes, n);
    usleep(ms * 1000);
  }
  return 0;
}
