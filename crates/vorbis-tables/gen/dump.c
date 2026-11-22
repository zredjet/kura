/*
 * vorbis-tables の生成の道具：aoTuV（aotuv_lancer_vorbis_sys 0.1.6 の vorbis_vendor）の表を JSON に書き出す。
 *
 * gen.py が作る 3 つの .inc を読み、手を入れていない C のヘッダ（modes/*.h・books/*.h・masking.h）と window.c を
 * そのまま #include して組む。値はコンパイラが初期化子から作ったビットのまま書く（浮動小数はビット列の 16 進）。
 *
 * - vorbisenc_head.inc：vorbisenc.c の先頭から setup_list の定義の終わりまで（型・_mode_template・_map_nominal・
 *   modes/ の #include・setup_list）。gen.py が文字列のまま切り出す。
 * - extracted.inc：floor1.c・psy.c・envelope.c の static の表（名前の衝突を避けるため改名だけする）。
 * - symtab.inc：上のすべての static const の定義の {名前, ファイル, 番地, 大きさ}。ポインタから名前と要素の数を引く。
 *
 * setup_list を先頭からたどり、5.1 の ve_setup_44_51 だけを除いて、テンプレートが指すすべての構造体・配列・
 * codebook を名前つきで書く。codebook は libvorbis の vorbis_staticbook_pack（codebook.c）で詰めたバイト列と
 * ビット数も書く（books.bin の正）。
 *
 * これは生成の道具で、kura のビルドには入らない（生成物をコミットする）。
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>

#include "vorbisenc_head.inc"
#include "masking.h"
#include "window.c"
#include "extracted.inc"
#include "symtab.inc"

/* ---- 名前の表 ---------------------------------------------------------- */

static const struct sym *find_sym(const void *p) {
  size_t i;
  const char *q = (const char *)p;
  for (i = 0; i < sizeof(SYMS) / sizeof(SYMS[0]); i++) {
    const char *b = (const char *)SYMS[i].p;
    if (q >= b && q < b + SYMS[i].size) {
      if (q != b) {
        fprintf(stderr, "配列の途中を指すポインタ（%s + %ld）は扱わない\n", SYMS[i].name, (long)(q - b));
        exit(1);
      }
      return &SYMS[i];
    }
  }
  fprintf(stderr, "名前のない番地 %p\n", p);
  exit(1);
}

/* ---- 書き出す配列の待ち行列 ------------------------------------------------ */

enum kind {
  K_F32, K_F64, K_I32, K_ATT3, K_VPADJ, K_NOISEGUARD, K_NOISE3, K_COMPAND, K_ADJSTEREO,
  K_PSYGLOBAL, K_FLOOR1, K_RESIDUE0, K_BOOKBLOCK, K_RESTEMPLATE, K_MAPTEMPLATE, K_MAPPING0,
  K_BOOKLIST, K_BOOKLISTLIST, K_INTPTRLIST, K_BOOK, K_PSY, K_MODE, K_AOTUVPRESET
};
static const char *KIND_NAME[] = {
  "f32", "f64", "i32", "Att3", "VpAdjblock", "Noiseguard", "Noise3", "Compandblock", "AdjStereo",
  "VorbisInfoPsyGlobal", "VorbisInfoFloor1", "VorbisInfoResidue0", "StaticBookblock",
  "VorbisResidueTemplate", "VorbisMappingTemplate", "VorbisInfoMapping0",
  "BookList", "BookListList", "IntArrayList", "Book", "VorbisInfoPsy", "VorbisInfoMode", "AotuvPreset"
};
static size_t elem_size(enum kind k) {
  switch (k) {
  case K_F32: return sizeof(float);
  case K_F64: return sizeof(double);
  case K_I32: return sizeof(int);
  case K_ATT3: return sizeof(att3);
  case K_VPADJ: return sizeof(vp_adjblock);
  case K_NOISEGUARD: return sizeof(noiseguard);
  case K_NOISE3: return sizeof(noise3);
  case K_COMPAND: return sizeof(compandblock);
  case K_ADJSTEREO: return sizeof(adj_stereo);
  case K_PSYGLOBAL: return sizeof(vorbis_info_psy_global);
  case K_FLOOR1: return sizeof(vorbis_info_floor1);
  case K_RESIDUE0: return sizeof(vorbis_info_residue0);
  case K_BOOKBLOCK: return sizeof(static_bookblock);
  case K_RESTEMPLATE: return sizeof(vorbis_residue_template);
  case K_MAPTEMPLATE: return sizeof(vorbis_mapping_template);
  case K_MAPPING0: return sizeof(vorbis_info_mapping0);
  case K_BOOKLIST: return sizeof(const static_codebook *);
  case K_BOOKLISTLIST: return sizeof(const static_codebook *const *);
  case K_INTPTRLIST: return sizeof(const int *);
  case K_BOOK: return sizeof(static_codebook);
  case K_PSY: return sizeof(vorbis_info_psy);
  case K_MODE: return sizeof(vorbis_info_mode);
  case K_AOTUVPRESET: return sizeof(aotuv_preset);
  }
  return 0;
}

#define MAXQ 8192
static struct { const struct sym *s; enum kind k; } QUEUE[MAXQ];
static int nqueue = 0;

/* 名前を書き、まだなら待ち行列に入れる。NULL は null。 */
static void ref(const void *p, enum kind k) {
  int i;
  const struct sym *s;
  if (!p) {
    printf("null");
    return;
  }
  s = find_sym(p);
  printf("\"%s\"", s->name);
  for (i = 0; i < nqueue; i++) {
    if (QUEUE[i].s == s) {
      if (QUEUE[i].k != k) {
        fprintf(stderr, "%s を別の型で指している\n", s->name);
        exit(1);
      }
      return;
    }
  }
  if (nqueue >= MAXQ) {
    fprintf(stderr, "待ち行列があふれた\n");
    exit(1);
  }
  if (s->size % elem_size(k)) {
    fprintf(stderr, "%s の大きさ %zu が要素の大きさ %zu で割り切れない\n", s->name, s->size, elem_size(k));
    exit(1);
  }
  QUEUE[nqueue].s = s;
  QUEUE[nqueue].k = k;
  nqueue++;
}

/* ---- 値の書き方 ------------------------------------------------------------ */

static void f32v(float f) {
  uint32_t u;
  memcpy(&u, &f, 4);
  printf("\"0x%08x\"", u);
}
static void f64v(double d) {
  uint64_t u;
  memcpy(&u, &d, 8);
  printf("\"0x%016llx\"", (unsigned long long)u);
}
static void i32s(const int *v, int n) {
  int i;
  printf("[");
  for (i = 0; i < n; i++) printf("%s%d", i ? "," : "", v[i]);
  printf("]");
}
static void f32s(const float *v, int n) {
  int i;
  printf("[");
  for (i = 0; i < n; i++) {
    if (i) printf(",");
    f32v(v[i]);
  }
  printf("]");
}
static void f64s(const double *v, int n) {
  int i;
  printf("[");
  for (i = 0; i < n; i++) {
    if (i) printf(",");
    f64v(v[i]);
  }
  printf("]");
}

static void att3_v(const att3 *a) {
  printf("{\"att\":");
  i32s(a->att, P_NOISECURVES);
  printf(",\"boost\":");
  f32v(a->boost);
  printf(",\"decay\":");
  f32v(a->decay);
  printf("}");
}
static void vpadj_v(const vp_adjblock *a) {
  printf("{\"block\":");
  i32s(a->block, P_BANDS);
  printf("}");
}
static void noiseguard_v(const noiseguard *a) {
  printf("{\"lo\":%d,\"hi\":%d,\"fixed\":%d}", a->lo, a->hi, a->fixed);
}
static void noise3_v(const noise3 *a) {
  int j;
  printf("{\"data\":[");
  for (j = 0; j < P_NOISECURVES; j++) {
    if (j) printf(",");
    i32s(a->data[j], 17);
  }
  printf("]}");
}
static void compand_v(const compandblock *a) {
  printf("{\"data\":");
  i32s(a->data, NOISE_COMPAND_LEVELS);
  printf("}");
}
static void adjstereo_v(const adj_stereo *a) {
  printf("{\"pre\":");
  i32s(a->pre, PACKETBLOBS);
  printf(",\"post\":");
  i32s(a->post, PACKETBLOBS);
  printf(",\"kHz\":");
  f32s(a->kHz, PACKETBLOBS);
  printf(",\"lowpasskHz\":");
  f32s(a->lowpasskHz, PACKETBLOBS);
  printf("}");
}
static void psyglobal_v(const vorbis_info_psy_global *g) {
  printf("{\"eighth_octave_lines\":%d", g->eighth_octave_lines);
  printf(",\"preecho_thresh\":");
  f32s(g->preecho_thresh, VE_BANDS);
  printf(",\"postecho_thresh\":");
  f32s(g->postecho_thresh, VE_BANDS);
  printf(",\"stretch_penalty\":");
  f32v(g->stretch_penalty);
  printf(",\"preecho_minenergy\":");
  f32v(g->preecho_minenergy);
  printf(",\"ampmax_att_per_sec\":");
  f32v(g->ampmax_att_per_sec);
  printf(",\"coupling_pkHz\":");
  i32s(g->coupling_pkHz, PACKETBLOBS);
  printf(",\"coupling_pointlimit\":[");
  i32s(g->coupling_pointlimit[0], PACKETBLOBS);
  printf(",");
  i32s(g->coupling_pointlimit[1], PACKETBLOBS);
  printf("],\"coupling_prepointamp\":");
  i32s(g->coupling_prepointamp, PACKETBLOBS);
  printf(",\"coupling_postpointamp\":");
  i32s(g->coupling_postpointamp, PACKETBLOBS);
  printf(",\"sliding_lowpass\":[");
  i32s(g->sliding_lowpass[0], PACKETBLOBS);
  printf(",");
  i32s(g->sliding_lowpass[1], PACKETBLOBS);
  printf("]}");
}
static void floor1_v(const vorbis_info_floor1 *f) {
  int j;
  printf("{\"partitions\":%d,\"partitionclass\":", f->partitions);
  i32s(f->partitionclass, VIF_PARTS);
  printf(",\"class_dim\":");
  i32s(f->class_dim, VIF_CLASS);
  printf(",\"class_subs\":");
  i32s(f->class_subs, VIF_CLASS);
  printf(",\"class_book\":");
  i32s(f->class_book, VIF_CLASS);
  printf(",\"class_subbook\":[");
  for (j = 0; j < VIF_CLASS; j++) {
    if (j) printf(",");
    i32s(f->class_subbook[j], 8);
  }
  printf("],\"mult\":%d,\"postlist\":", f->mult);
  i32s(f->postlist, VIF_POSIT + 2);
  printf(",\"maxover\":");
  f32v(f->maxover);
  printf(",\"maxunder\":");
  f32v(f->maxunder);
  printf(",\"maxerr\":");
  f32v(f->maxerr);
  printf(",\"twofitweight\":");
  f32v(f->twofitweight);
  printf(",\"twofitatten\":");
  f32v(f->twofitatten);
  printf(",\"n\":%d}", f->n);
}
static void residue0_v(const vorbis_info_residue0 *r) {
  printf("{\"begin\":%ld,\"end\":%ld,\"grouping\":%d,\"partitions\":%d,\"partvals\":%d,\"groupbook\":%d",
         r->begin, r->end, r->grouping, r->partitions, r->partvals, r->groupbook);
  printf(",\"secondstages\":");
  i32s(r->secondstages, 64);
  printf(",\"booklist\":");
  i32s(r->booklist, 512);
  printf(",\"classmetric1\":");
  i32s(r->classmetric1, 64);
  printf(",\"classmetric2\":");
  i32s(r->classmetric2, 64);
  printf("}");
}
static void bookblock_v(const static_bookblock *b) {
  int i, k;
  printf("{\"books\":[");
  for (i = 0; i < 12; i++) {
    printf("%s[", i ? "," : "");
    for (k = 0; k < 4; k++) {
      if (k) printf(",");
      ref(b->books[i][k], K_BOOK);
    }
    printf("]");
  }
  printf("]}");
}
static void restemplate_v(const vorbis_residue_template *t) {
  printf("{\"res_type\":%d,\"limit_type\":%d,\"grouping\":%d,\"res\":", t->res_type, t->limit_type, t->grouping);
  ref(t->res, K_RESIDUE0);
  printf(",\"book_aux\":");
  ref(t->book_aux, K_BOOK);
  printf(",\"book_aux_managed\":");
  ref(t->book_aux_managed, K_BOOK);
  printf(",\"books_base\":");
  ref(t->books_base, K_BOOKBLOCK);
  printf(",\"books_base_managed\":");
  ref(t->books_base_managed, K_BOOKBLOCK);
  printf("}");
}
static void maptemplate_v(const vorbis_mapping_template *t) {
  printf("{\"map\":");
  ref(t->map, K_MAPPING0);
  printf(",\"res\":");
  ref(t->res, K_RESTEMPLATE);
  printf("}");
}
static void mapping0_v(const vorbis_info_mapping0 *m) {
  printf("{\"submaps\":%d,\"chmuxlist\":", m->submaps);
  i32s(m->chmuxlist, 256);
  printf(",\"floorsubmap\":");
  i32s(m->floorsubmap, 16);
  printf(",\"residuesubmap\":");
  i32s(m->residuesubmap, 16);
  printf(",\"coupling_steps\":%d,\"coupling_mag\":", m->coupling_steps);
  i32s(m->coupling_mag, 256);
  printf(",\"coupling_ang\":");
  i32s(m->coupling_ang, 256);
  printf("}");
}
static void psy_v(const vorbis_info_psy *p) {
  int j;
  printf("{\"blockflag\":%d", p->blockflag);
  printf(",\"ath_adjatt\":");
  f32v(p->ath_adjatt);
  printf(",\"ath_maxatt\":");
  f32v(p->ath_maxatt);
  printf(",\"tone_masteratt\":");
  f32s(p->tone_masteratt, P_NOISECURVES);
  printf(",\"tone_centerboost\":");
  f32v(p->tone_centerboost);
  printf(",\"tone_decay\":");
  f32v(p->tone_decay);
  printf(",\"tone_abs_limit\":");
  f32v(p->tone_abs_limit);
  printf(",\"toneatt\":");
  f32s(p->toneatt, P_BANDS);
  printf(",\"noisemaskp\":%d", p->noisemaskp);
  printf(",\"noisemaxsupp\":");
  f32v(p->noisemaxsupp);
  printf(",\"noisewindowlo\":");
  f32v(p->noisewindowlo);
  printf(",\"noisewindowhi\":");
  f32v(p->noisewindowhi);
  printf(",\"noisewindowlomin\":%d,\"noisewindowhimin\":%d,\"noisewindowfixed\":%d",
         p->noisewindowlomin, p->noisewindowhimin, p->noisewindowfixed);
  printf(",\"noiseoff\":[");
  for (j = 0; j < P_NOISECURVES; j++) {
    if (j) printf(",");
    f32s(p->noiseoff[j], P_BANDS);
  }
  printf("],\"noisecompand\":");
  f32s(p->noisecompand, NOISE_COMPAND_LEVELS);
  printf(",\"noisecompand_high\":");
  f32s(p->noisecompand_high, NOISE_COMPAND_LEVELS);
  printf(",\"flacint\":");
  f32v(p->flacint);
  printf(",\"max_curve_dB\":");
  f32v(p->max_curve_dB);
  printf(",\"normal_p\":%d,\"normal_start\":%d,\"normal_partition\":%d,\"normal_thresh\":",
         p->normal_p, p->normal_start, p->normal_partition);
  f64v(p->normal_thresh);
  printf("}");
}
static void mode_v(const vorbis_info_mode *m) {
  printf("{\"blockflag\":%d,\"windowtype\":%d,\"transformtype\":%d,\"mapping\":%d}",
         m->blockflag, m->windowtype, m->transformtype, m->mapping);
}
static void aotuvpreset_v(const aotuv_preset *a) {
  printf("{\"tonecomp_endp\":%d,\"tonecomp_thres\":", a->tonecomp_endp);
  f32v(a->tonecomp_thres);
  printf(",\"min_nn_lp\":%d,\"tonefix_end\":%d}", a->min_nn_lp, a->tonefix_end);
}

/* codebook：静的な値と、vorbis_staticbook_pack で詰めたバイト列 */
static void book_v(const static_codebook *c) {
  long i, quantvals = 0;
  oggpack_buffer opb;
  printf("{\"dim\":%ld,\"entries\":%ld,\"lengthlist\":[", c->dim, c->entries);
  for (i = 0; i < c->entries; i++) printf("%s%d", i ? "," : "", (int)(unsigned char)c->lengthlist[i]);
  printf("],\"maptype\":%d,\"q_min\":%ld,\"q_delta\":%ld,\"q_quant\":%d,\"q_sequencep\":%d,\"quantlist\":[",
         c->maptype, c->q_min, c->q_delta, c->q_quant, c->q_sequencep);
  if (c->maptype == 1) quantvals = _book_maptype1_quantvals(c);
  if (c->maptype == 2) quantvals = c->entries * c->dim;
  for (i = 0; i < quantvals; i++) printf("%s%ld", i ? "," : "", c->quantlist[i]);
  printf("]");
  oggpack_writeinit(&opb);
  if (vorbis_staticbook_pack(c, &opb)) {
    fprintf(stderr, "vorbis_staticbook_pack が失敗した\n");
    exit(1);
  }
  printf(",\"bits\":%ld,\"packed\":\"", oggpack_bits(&opb));
  for (i = 0; i < oggpack_bytes(&opb); i++) printf("%02x", opb.buffer[i]);
  printf("\"}");
  oggpack_writeclear(&opb);
}

/* 待ち行列の 1 つを書く */
static void emit(int qi) {
  const struct sym *s = QUEUE[qi].s;
  enum kind k = QUEUE[qi].k;
  size_t n = s->size / elem_size(k), i;
  printf("\"%s\":{\"kind\":\"%s\",\"file\":\"%s\",\"count\":%zu,\"values\":[", s->name, KIND_NAME[k], s->file, n);
  for (i = 0; i < n; i++) {
    if (i) printf(",");
    switch (k) {
    case K_F32: f32v(((const float *)s->p)[i]); break;
    case K_F64: f64v(((const double *)s->p)[i]); break;
    case K_I32: printf("%d", ((const int *)s->p)[i]); break;
    case K_ATT3: att3_v((const att3 *)s->p + i); break;
    case K_VPADJ: vpadj_v((const vp_adjblock *)s->p + i); break;
    case K_NOISEGUARD: noiseguard_v((const noiseguard *)s->p + i); break;
    case K_NOISE3: noise3_v((const noise3 *)s->p + i); break;
    case K_COMPAND: compand_v((const compandblock *)s->p + i); break;
    case K_ADJSTEREO: adjstereo_v((const adj_stereo *)s->p + i); break;
    case K_PSYGLOBAL: psyglobal_v((const vorbis_info_psy_global *)s->p + i); break;
    case K_FLOOR1: floor1_v((const vorbis_info_floor1 *)s->p + i); break;
    case K_RESIDUE0: residue0_v((const vorbis_info_residue0 *)s->p + i); break;
    case K_BOOKBLOCK: bookblock_v((const static_bookblock *)s->p + i); break;
    case K_RESTEMPLATE: restemplate_v((const vorbis_residue_template *)s->p + i); break;
    case K_MAPTEMPLATE: maptemplate_v((const vorbis_mapping_template *)s->p + i); break;
    case K_MAPPING0: mapping0_v((const vorbis_info_mapping0 *)s->p + i); break;
    case K_BOOKLIST: ref(((const static_codebook *const *)s->p)[i], K_BOOK); break;
    case K_BOOKLISTLIST: ref(((const static_codebook *const *const *)s->p)[i], K_BOOKLIST); break;
    case K_INTPTRLIST: ref(((const int *const *)s->p)[i], K_I32); break;
    case K_BOOK: book_v((const static_codebook *)s->p + i); break;
    case K_PSY: psy_v((const vorbis_info_psy *)s->p + i); break;
    case K_MODE: mode_v((const vorbis_info_mode *)s->p + i); break;
    case K_AOTUVPRESET: aotuvpreset_v((const aotuv_preset *)s->p + i); break;
    }
  }
  printf("]}");
}

static void template_v(const ve_setup_data_template *t) {
  const struct sym *s = find_sym(t);
  printf("{\"name\":\"%s\",\"file\":\"%s\"", s->name, s->file);
  printf(",\"mappings\":%d", t->mappings);
  printf(",\"rate_mapping\":"); ref(t->rate_mapping, K_F64);
  printf(",\"quality_mapping\":"); ref(t->quality_mapping, K_F64);
  printf(",\"pre_amp\":"); ref(t->pre_amp, K_F32);
  printf(",\"coupling_restriction\":%d", t->coupling_restriction);
  printf(",\"samplerate_min_restriction\":%ld", t->samplerate_min_restriction);
  printf(",\"samplerate_max_restriction\":%ld", t->samplerate_max_restriction);
  printf(",\"blocksize_short\":"); ref(t->blocksize_short, K_I32);
  printf(",\"blocksize_long\":"); ref(t->blocksize_long, K_I32);
  printf(",\"psy_tone_masteratt\":"); ref(t->psy_tone_masteratt, K_ATT3);
  printf(",\"psy_tone_0dB\":"); ref(t->psy_tone_0dB, K_I32);
  printf(",\"psy_tone_dBsuppress\":"); ref(t->psy_tone_dBsuppress, K_I32);
  printf(",\"psy_tone_adj_impulse\":"); ref(t->psy_tone_adj_impulse, K_VPADJ);
  printf(",\"psy_tone_adj_long\":"); ref(t->psy_tone_adj_long, K_VPADJ);
  printf(",\"psy_tone_adj_other\":"); ref(t->psy_tone_adj_other, K_VPADJ);
  printf(",\"psy_noiseguards\":"); ref(t->psy_noiseguards, K_NOISEGUARD);
  printf(",\"psy_noise_bias_impulse\":"); ref(t->psy_noise_bias_impulse, K_NOISE3);
  printf(",\"psy_noise_bias_padding\":"); ref(t->psy_noise_bias_padding, K_NOISE3);
  printf(",\"psy_noise_bias_trans\":"); ref(t->psy_noise_bias_trans, K_NOISE3);
  printf(",\"psy_noise_bias_long\":"); ref(t->psy_noise_bias_long, K_NOISE3);
  printf(",\"psy_noise_dBsuppress\":"); ref(t->psy_noise_dBsuppress, K_I32);
  printf(",\"psy_noise_compand\":"); ref(t->psy_noise_compand, K_COMPAND);
  printf(",\"psy_noise_compand_short_mapping\":"); ref(t->psy_noise_compand_short_mapping, K_F64);
  printf(",\"psy_noise_compand_long_mapping\":"); ref(t->psy_noise_compand_long_mapping, K_F64);
  printf(",\"psy_noise_normal_start\":["); ref(t->psy_noise_normal_start[0], K_I32);
  printf(","); ref(t->psy_noise_normal_start[1], K_I32);
  printf("],\"psy_noise_normal_partition\":["); ref(t->psy_noise_normal_partition[0], K_I32);
  printf(","); ref(t->psy_noise_normal_partition[1], K_I32);
  printf("],\"psy_noise_normal_thresh\":"); ref(t->psy_noise_normal_thresh, K_F64);
  printf(",\"psy_ath_float\":"); ref(t->psy_ath_float, K_I32);
  printf(",\"psy_ath_abs\":"); ref(t->psy_ath_abs, K_I32);
  printf(",\"psy_lowpass\":"); ref(t->psy_lowpass, K_F64);
  printf(",\"global_params\":"); ref(t->global_params, K_PSYGLOBAL);
  printf(",\"global_mapping\":"); ref(t->global_mapping, K_F64);
  printf(",\"stereo_modes\":"); ref(t->stereo_modes, K_ADJSTEREO);
  printf(",\"floor_books\":"); ref(t->floor_books, K_BOOKLISTLIST);
  printf(",\"floor_params\":"); ref(t->floor_params, K_FLOOR1);
  printf(",\"floor_mappings\":%d", t->floor_mappings);
  printf(",\"floor_mapping_list\":"); ref(t->floor_mapping_list, K_INTPTRLIST);
  printf(",\"maps\":"); ref(t->maps, K_MAPTEMPLATE);
  printf("}");
}

int main(void) {
  int i, first;

  /* psy.c と floor1.c の FLOOR1_fromdB_LOOKUP は同じ値（1 つだけ書く） */
  if (memcmp(FLOOR1_fromdB_LOOKUP__floor1, FLOOR1_fromdB_LOOKUP__psy, sizeof(FLOOR1_fromdB_LOOKUP__floor1))) {
    fprintf(stderr, "floor1.c と psy.c の FLOOR1_fromdB_LOOKUP が違う\n");
    exit(1);
  }

  printf("{\"templates\":[");
  first = 1;
  for (i = 0; setup_list[i]; i++) {
    if (setup_list[i] == &ve_setup_44_51) continue; /* 5.1 は移さない（6 チャンネルも uncoupled で書く） */
    if (!first) printf(",");
    first = 0;
    template_v(setup_list[i]);
  }
  printf("],\"setup_list_len\":%d", i);

  /* テンプレートからは指されないが、エンコーダが使う表 */
  printf(",\"extra\":[");
  ref(&_psy_info_template, K_PSY); printf(",");
  ref(_mode_template, K_MODE); printf(",");
  ref(_map_nominal, K_MAPPING0); printf(",");
  ref(ATH, K_F32); printf(",");
  ref(tonemasks, K_F32); printf(",");
  ref(vwin64, K_F32); printf(",");
  ref(vwin128, K_F32); printf(",");
  ref(vwin256, K_F32); printf(",");
  ref(vwin512, K_F32); printf(",");
  ref(vwin1024, K_F32); printf(",");
  ref(vwin2048, K_F32); printf(",");
  ref(vwin4096, K_F32); printf(",");
  ref(vwin8192, K_F32); printf(",");
  ref(FLOOR1_fromdB_LOOKUP__floor1, K_F32); printf(",");
  ref(stereo_threshholds, K_F64); printf(",");
  ref(stereo_threshholds_X, K_F64); printf(",");
  ref(set_aotuv_psy, K_AOTUVPRESET); printf(",");
  ref(band_begin, K_I32); printf(",");
  ref(band_end, K_I32); printf(",");
  /* aoTuV の M3・M7・M9 の表（psy.c） */
  ref(m3n32, K_I32); printf(",");
  ref(m3n44, K_I32); printf(",");
  ref(m3n48, K_I32); printf(",");
  ref(m3n32x2, K_I32); printf(",");
  ref(m3n44x2, K_I32); printf(",");
  ref(m3n48x2, K_I32); printf(",");
  ref(freq_bfn128, K_I32); printf(",");
  ref(freq_bfn256, K_I32); printf(",");
  ref(stn_compand, K_I32); printf(",");
  ref(ntfix_offset, K_F32);
  printf("]");

  /* 指されたものを順に書く（書く途中で指されたものも待ち行列の後ろに入る） */
  printf(",\"symbols\":{");
  for (i = 0; i < nqueue; i++) {
    if (i) printf(",");
    emit(i);
  }
  printf("}}\n");
  return 0;
}
