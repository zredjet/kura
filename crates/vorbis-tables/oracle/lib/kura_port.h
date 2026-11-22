/* 照合用のエンコーダ（KURA_PORT）：ブロックごとの中間値の書き出し（ダンプ）。
   形式は oracle/README.md の「ダンプの形式」。KURA_PORT でないビルドでは何もしない。 */
#ifndef KURA_PORT_H
#define KURA_PORT_H

#ifdef KURA_PORT
#include <stdint.h>

/* 0：書かない、1：floor・residue の整数など、2：1 に加えて浮動小数の途中の値 */
extern int kura_dump_level;
/* 今のブロックの通し番号（0 から。mapping0_forward の終わりで 1 進む） */
extern int64_t kura_dump_seq;
/* 今書いているチャンネル（floor1_encode の FOUT）または submap（RCLS）。mapping0.c が入れる */
extern int kura_dump_ch;

/* path を開いてレベルを決める。path が NULL か level<=0 なら書かない。成功で 0。 */
int  kd_open(const char *path, int level);
void kd_close(void);

/* 記録：tag は 4 文字の ASCII、ch はチャンネル（ブロック全体の記録は KD_NOCH） */
#define KD_NOCH 0xFFFF
void kd_i64(const char *tag, int ch, const int64_t *v, long n);
void kd_i32(const char *tag, int ch, const int *v, long n);
void kd_long(const char *tag, int ch, const long *v, long n); /* i32 として書く */
void kd_f32(const char *tag, int ch, const float *v, long n); /* ビット列（u32）のまま */

/* 有効な KURA_ABL のビット（abl.h の ABL_*） */
int kura_abl_mask(void);

#define KD1 (kura_dump_level>=1)
#define KD2 (kura_dump_level>=2)

#else
#define KD1 0
#define KD2 0
#endif

#endif
