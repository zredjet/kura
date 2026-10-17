/* 照合用のエンコーダ（KURA_PORT）：超越関数を Rust の libm =0.2.16 に差し替える。
   clang の -include で、すべての翻訳単位の先頭に読ませる。
   先に <math.h> を読んでおくので、ソースがあとで <math.h> を読んでも（インクルードガードで）
   宣言は増えず、下のマクロがそのまま効く。関数形式のマクロなので、変数名（sharedbook.c の
   `exp` など）は置き換わらない。
   sqrt・floor・ceil・fabs・rint は IEEE で一意に決まるので差し替えない（rint は既定の
   丸めの向き＝最近接偶数のまま）。 */
#ifndef KURA_LIBM_H
#define KURA_LIBM_H

#include <math.h>

double kura_exp(double);
double kura_log(double);
double kura_pow(double, double);
double kura_atan(double);
double kura_sin(double);
double kura_cos(double);
double kura_acos(double);
double kura_hypot(double, double);
double kura_ldexp(double, int);
double kura_frexp(double, int *);

#define exp(x)      kura_exp(x)
#define log(x)      kura_log(x)
#define pow(x, y)   kura_pow((x), (y))
#define atan(x)     kura_atan(x)
#define sin(x)      kura_sin(x)
#define cos(x)      kura_cos(x)
#define acos(x)     kura_acos(x)
#define hypot(x, y) kura_hypot((x), (y))
#define ldexp(x, e) kura_ldexp((x), (e))
#define frexp(x, e) kura_frexp((x), (e))

/* ソースが使わない超越関数は、使ったらコンパイルで止める（差し替え漏れを防ぐ）。 */
#pragma GCC poison expf logf powf sinf cosf tan tanf atanf atan2 atan2f acosf asin asinf
#pragma GCC poison log10 log10f log2 log2f log1p exp2 exp2f expm1 sinh cosh tanh
#pragma GCC poison hypotf ldexpf frexpf cbrt erf lgamma tgamma

#endif
