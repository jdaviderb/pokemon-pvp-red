#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
// retro_log_printf_t: void(*)(enum retro_log_level level, const char *fmt, ...)
void n64probe_log(int level, const char *fmt, ...) {
    if (!getenv("PROBE_VERBOSE")) return;   // silent unless verbose
    va_list ap; va_start(ap, fmt);
    fprintf(stderr, "[core log %d] ", level);
    vfprintf(stderr, fmt, ap);
    va_end(ap);
}
