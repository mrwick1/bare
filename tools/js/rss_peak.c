// Run a command with its output discarded and print its peak RSS in KiB (from wait4). The exit status is the command's.
// A small C parent is needed: a child keeps the peak RSS of the process it was forked from through `exec`.
//   rss_peak COMMAND [ARGS...]
#include <fcntl.h>
#include <stdio.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <unistd.h>

int main(int argc, char **argv) {
    if (argc < 2) return 2;
    pid_t p = fork();
    if (p == 0) {
        int n = open("/dev/null", O_WRONLY);
        dup2(n, 1);
        dup2(n, 2);
        execvp(argv[1], argv + 1);
        _exit(127);
    }
    int st;
    struct rusage ru;
    if (p < 0 || wait4(p, &st, 0, &ru) < 0) return 1;
    printf("%ld\n", ru.ru_maxrss);
    return WIFEXITED(st) ? WEXITSTATUS(st) : 1;
}
