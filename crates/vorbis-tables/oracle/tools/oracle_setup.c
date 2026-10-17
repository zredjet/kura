/* 照合用のエンコーダの設定（vorbis_encode_init_vbr のあとの codec_setup_info）を JSON で出す。
   使い方: oracle_setup rate channels q [--look] [--bits]
     --look : vorbis_analysis_init が作る導出の表（psy の look・floor の look・MDCT と FFT の三角関数・
              envelope・窓）も出す
     --bits : 浮動小数を数ではなくビット列の文字列（f32 は "0x%08x"、f64 は "0x%016llx"）で出す
   既定の浮動小数の書き方：f32 は %.9g、f64 は %.17g（どちらも元の値に戻せる桁数）。
   NaN・無限大は文字列 "nan"・"inf"・"-inf"。 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <vorbis/vorbisenc.h>
#include "codec_internal.h"
#include "backends.h"
#include "codebook.h"
#include "envelope.h"
#include "mdct.h"
#include "smallft.h"
#include "window.h"
#include "kura_port.h"

typedef struct { const static_codebook *book; const char *name; const char *file; } kura_bookname;
extern const int kura_booknames_count;
extern const kura_bookname kura_booknames[];
extern const char *kura_setup_template(const void *setup, int *coupling_restriction,
                                       long *rate_min, long *rate_max, int *mappings);

static int bits_mode=0;

/* ---- JSON の小さな書き手 ---- */
static int first[64]; static int is_arr[64]; static int depth=0;
static void indent(void){ putchar('\n'); for(int i=0;i<depth;i++)fputs("  ",stdout); }
/* 配列の中身は改行しない（オブジェクトの中のキーごとに改行する） */
static void sep(const char *key){
  if(!first[depth])putchar(',');
  first[depth]=0;
  if(!is_arr[depth])indent();
  if(key)printf("\"%s\":",key);
}
static void obj_begin(const char *key){ sep(key); putchar('{'); depth++; first[depth]=1; is_arr[depth]=0; }
static void obj_end(void){ depth--; indent(); putchar('}'); }
static void arr_begin(const char *key){ sep(key); putchar('['); depth++; first[depth]=1; is_arr[depth]=1; }
static void arr_end(void){ depth--; putchar(']'); }
static void v_f32(float v){
  if(bits_mode){ uint32_t u; memcpy(&u,&v,4); printf("\"0x%08x\"",u); return; }
  if(isnan(v)){ fputs("\"nan\"",stdout); return; }
  if(isinf(v)){ fputs(v>0?"\"inf\"":"\"-inf\"",stdout); return; }
  printf("%.9g",(double)v);
}
static void v_f64(double v){
  if(bits_mode){ uint64_t u; memcpy(&u,&v,8); printf("\"0x%016llx\"",(unsigned long long)u); return; }
  if(isnan(v)){ fputs("\"nan\"",stdout); return; }
  if(isinf(v)){ fputs(v>0?"\"inf\"":"\"-inf\"",stdout); return; }
  printf("%.17g",v);
}
static void kv_int(const char *k, long v){ sep(k); printf("%ld",v); }
static void kv_str(const char *k, const char *s){ sep(k); printf("\"%s\"",s); }
static void kv_f32(const char *k, float v){ sep(k); v_f32(v); }
static void kv_f64(const char *k, double v){ sep(k); v_f64(v); }
static void kv_ints(const char *k, const int *v, long n){ arr_begin(k); for(long i=0;i<n;i++){ sep(NULL); printf("%d",v[i]); } arr_end(); }
static void kv_longs(const char *k, const long *v, long n){ arr_begin(k); for(long i=0;i<n;i++){ sep(NULL); printf("%ld",v[i]); } arr_end(); }
static void kv_f32s(const char *k, const float *v, long n){ arr_begin(k); for(long i=0;i<n;i++){ sep(NULL); v_f32(v[i]); } arr_end(); }
static void kv_f64s(const char *k, const double *v, long n){ arr_begin(k); for(long i=0;i<n;i++){ sep(NULL); v_f64(v[i]); } arr_end(); }

static const char *book_name(const static_codebook *b, const char **file){
  for(int i=0;i<kura_booknames_count;i++)
    if(kura_booknames[i].book==b){ if(file)*file=kura_booknames[i].file; return kura_booknames[i].name; }
  if(file)*file="?";
  return "?";
}

static void psy_info(const vorbis_info_psy *p){
  obj_begin(NULL);
  kv_int("blockflag",p->blockflag);
  kv_f32("ath_adjatt",p->ath_adjatt);
  kv_f32("ath_maxatt",p->ath_maxatt);
  kv_f32s("tone_masteratt",p->tone_masteratt,P_NOISECURVES);
  kv_f32("tone_centerboost",p->tone_centerboost);
  kv_f32("tone_decay",p->tone_decay);
  kv_f32("tone_abs_limit",p->tone_abs_limit);
  kv_f32s("toneatt",p->toneatt,P_BANDS);
  kv_int("noisemaskp",p->noisemaskp);
  kv_f32("noisemaxsupp",p->noisemaxsupp);
  kv_f32("noisewindowlo",p->noisewindowlo);
  kv_f32("noisewindowhi",p->noisewindowhi);
  kv_int("noisewindowlomin",p->noisewindowlomin);
  kv_int("noisewindowhimin",p->noisewindowhimin);
  kv_int("noisewindowfixed",p->noisewindowfixed);
  arr_begin("noiseoff"); for(int i=0;i<P_NOISECURVES;i++)kv_f32s(NULL,p->noiseoff[i],P_BANDS); arr_end();
  kv_f32s("noisecompand",p->noisecompand,NOISE_COMPAND_LEVELS);
  kv_f32s("noisecompand_high",p->noisecompand_high,NOISE_COMPAND_LEVELS);
  kv_f32("flacint",p->flacint);
  kv_f32("max_curve_dB",p->max_curve_dB);
  kv_int("normal_p",p->normal_p);
  kv_int("normal_start",p->normal_start);
  kv_int("normal_partition",p->normal_partition);
  kv_f64("normal_thresh",p->normal_thresh);
  obj_end();
}

static void mdct_look(const char *k, const mdct_lookup *m){
  obj_begin(k);
  kv_int("n",m->n); kv_int("log2n",m->log2n);
  kv_f32s("trig",m->trig,m->n+m->n/4);
  kv_ints("bitrev",m->bitrev,m->n/4);
  kv_f32("scale",m->scale);
  obj_end();
}

int main(int argc, char **argv){
  if(argc<4){ fprintf(stderr,"usage: oracle_setup rate channels q [--look] [--bits]\n"); return 2; }
  long rate=atol(argv[1]); int ch=atoi(argv[2]); int q=atoi(argv[3]);
  int look=0;
  for(int a=4;a<argc;a++){
    if(!strcmp(argv[a],"--look"))look=1;
    else if(!strcmp(argv[a],"--bits"))bits_mode=1;
    else { fprintf(stderr,"unknown option %s\n",argv[a]); return 2; }
  }
  vorbis_info vi; vorbis_info_init(&vi);
  float quality=(float)q/10.0f;
  int ret=vorbis_encode_init_vbr(&vi,ch,rate,quality);
  if(ret){ fprintf(stderr,"vorbis_encode_init_vbr failed: %d\n",ret); return 1; }
  codec_setup_info *ci=vi.codec_setup;
  highlevel_encode_setup *hi=&ci->hi;

  first[0]=1; depth=0;
  putchar('{'); depth=1; first[1]=1; is_arr[1]=0;

  obj_begin("input");
  kv_int("rate",rate); kv_int("channels",ch); kv_int("q",q); kv_f32("quality",quality);
  {
    static const char *names[]={"M1","M2","M3","M4","M5","M6","M7","M8","M9","ENV","PREAMP","HYPOT","ATH","NN","NOTONE","NOPOINT","LONGONLY","NOEXTRAP","ALLPOINT","NOCOMPAND",0};
    int m=kura_abl_mask();
    kv_int("abl_mask",m);
    arr_begin("abl"); for(int i=0;names[i];i++) if(m&(1<<i)){ sep(NULL); printf("\"%s\"",names[i]); } arr_end();
  }
  obj_end();

  obj_begin("vorbis_info");
  kv_int("version",vi.version); kv_int("channels",vi.channels); kv_int("rate",vi.rate);
  kv_int("bitrate_upper",vi.bitrate_upper); kv_int("bitrate_nominal",vi.bitrate_nominal);
  kv_int("bitrate_lower",vi.bitrate_lower); kv_int("bitrate_window",vi.bitrate_window);
  obj_end();

  {
    int cr=0,maps=0; long rmin=0,rmax=0;
    const char *tn=kura_setup_template(hi->setup,&cr,&rmin,&rmax,&maps);
    obj_begin("template");
    kv_str("name",tn); kv_int("coupling_restriction",cr);
    kv_int("samplerate_min",rmin); kv_int("samplerate_max",rmax); kv_int("mappings",maps);
    obj_end();
  }

  kv_longs("blocksizes",ci->blocksizes,2);
  kv_int("halfrate_flag",ci->halfrate_flag);
  kv_ints("block_lowpassr",ci->block_lowpassr,2);

  arr_begin("modes");
  for(int i=0;i<ci->modes;i++){
    vorbis_info_mode *m=ci->mode_param[i];
    obj_begin(NULL);
    kv_int("blockflag",m->blockflag); kv_int("windowtype",m->windowtype);
    kv_int("transformtype",m->transformtype); kv_int("mapping",m->mapping);
    obj_end();
  }
  arr_end();

  arr_begin("maps");
  for(int i=0;i<ci->maps;i++){
    vorbis_info_mapping0 *m=ci->map_param[i];
    obj_begin(NULL);
    kv_int("type",ci->map_type[i]);
    kv_int("submaps",m->submaps);
    kv_ints("chmuxlist",m->chmuxlist,ch);
    kv_ints("floorsubmap",m->floorsubmap,m->submaps);
    kv_ints("residuesubmap",m->residuesubmap,m->submaps);
    kv_int("coupling_steps",m->coupling_steps);
    kv_ints("coupling_mag",m->coupling_mag,m->coupling_steps);
    kv_ints("coupling_ang",m->coupling_ang,m->coupling_steps);
    obj_end();
  }
  arr_end();

  arr_begin("floors");
  for(int i=0;i<ci->floors;i++){
    obj_begin(NULL);
    kv_int("type",ci->floor_type[i]);
    if(ci->floor_type[i]==1){
      vorbis_info_floor1 *f=ci->floor_param[i];
      int classes=0, posts=2;
      for(int j=0;j<f->partitions;j++){
        if(f->partitionclass[j]+1>classes)classes=f->partitionclass[j]+1;
        posts+=f->class_dim[f->partitionclass[j]];
      }
      kv_int("partitions",f->partitions);
      kv_ints("partitionclass",f->partitionclass,f->partitions);
      kv_ints("class_dim",f->class_dim,classes);
      kv_ints("class_subs",f->class_subs,classes);
      kv_ints("class_book",f->class_book,classes);
      arr_begin("class_subbook");
      for(int c=0;c<classes;c++)kv_ints(NULL,f->class_subbook[c],1<<f->class_subs[c]);
      arr_end();
      kv_int("mult",f->mult);
      kv_int("posts",posts);
      kv_ints("postlist",f->postlist,posts);
      kv_f32("maxover",f->maxover); kv_f32("maxunder",f->maxunder); kv_f32("maxerr",f->maxerr);
      kv_f32("twofitweight",f->twofitweight); kv_f32("twofitatten",f->twofitatten);
      kv_int("n",f->n);
    }
    obj_end();
  }
  arr_end();

  arr_begin("residues");
  for(int i=0;i<ci->residues;i++){
    vorbis_info_residue0 *r=ci->residue_param[i];
    int nbooks=0;
    for(int j=0;j<r->partitions;j++){ unsigned s=r->secondstages[j]; while(s){ nbooks+=s&1; s>>=1; } }
    obj_begin(NULL);
    kv_int("type",ci->residue_type[i]);
    kv_int("begin",r->begin); kv_int("end",r->end);
    kv_int("grouping",r->grouping); kv_int("partitions",r->partitions);
    kv_int("partvals",r->partvals); kv_int("groupbook",r->groupbook);
    kv_ints("secondstages",r->secondstages,r->partitions);
    kv_ints("booklist",r->booklist,nbooks);
    kv_ints("classmetric1",r->classmetric1,r->partitions);
    kv_ints("classmetric2",r->classmetric2,r->partitions);
    obj_end();
  }
  arr_end();

  arr_begin("books");
  for(int i=0;i<ci->books;i++){
    const static_codebook *b=ci->book_param[i];
    const char *file; const char *nm=book_name(b,&file);
    long used=0; int maxlen=0;
    for(long e=0;e<b->entries;e++){ if(b->lengthlist[e]>0){ used++; if(b->lengthlist[e]>maxlen)maxlen=b->lengthlist[e]; } }
    obj_begin(NULL);
    kv_int("index",i); kv_str("name",nm); kv_str("file",file);
    kv_int("dim",b->dim); kv_int("entries",b->entries); kv_int("used_entries",used); kv_int("max_length",maxlen);
    kv_int("maptype",b->maptype);
    kv_int("q_min",b->q_min); kv_int("q_delta",b->q_delta); kv_int("q_quant",b->q_quant); kv_int("q_sequencep",b->q_sequencep);
    obj_end();
  }
  arr_end();

  arr_begin("psy");
  for(int i=0;i<ci->psys;i++)psy_info(ci->psy_param[i]);
  arr_end();

  {
    vorbis_info_psy_global *g=&ci->psy_g_param;
    obj_begin("psy_global");
    kv_int("eighth_octave_lines",g->eighth_octave_lines);
    kv_f32s("preecho_thresh",g->preecho_thresh,VE_BANDS);
    kv_f32s("postecho_thresh",g->postecho_thresh,VE_BANDS);
    kv_f32("stretch_penalty",g->stretch_penalty);
    kv_f32("preecho_minenergy",g->preecho_minenergy);
    kv_f32("ampmax_att_per_sec",g->ampmax_att_per_sec);
    kv_ints("coupling_pkHz",g->coupling_pkHz,PACKETBLOBS);
    arr_begin("coupling_pointlimit"); for(int j=0;j<2;j++)kv_ints(NULL,g->coupling_pointlimit[j],PACKETBLOBS); arr_end();
    kv_ints("coupling_prepointamp",g->coupling_prepointamp,PACKETBLOBS);
    kv_ints("coupling_postpointamp",g->coupling_postpointamp,PACKETBLOBS);
    arr_begin("sliding_lowpass"); for(int j=0;j<2;j++)kv_ints(NULL,g->sliding_lowpass[j],PACKETBLOBS); arr_end();
    kv_int("vbr_blob",PACKETBLOBS/2);
    obj_end();
  }

  obj_begin("highlevel");
  kv_int("set_in_stone",hi->set_in_stone);
  kv_f64("base_setting",hi->base_setting);
  kv_f64("impulse_noisetune",hi->impulse_noisetune);
  kv_f32("req",hi->req);
  kv_int("managed",hi->managed);
  kv_int("bitrate_min",hi->bitrate_min); kv_int("bitrate_av",hi->bitrate_av);
  kv_f64("bitrate_av_damp",hi->bitrate_av_damp); kv_int("bitrate_max",hi->bitrate_max);
  kv_int("bitrate_reservoir",hi->bitrate_reservoir); kv_f64("bitrate_reservoir_bias",hi->bitrate_reservoir_bias);
  kv_int("impulse_block_p",hi->impulse_block_p); kv_int("noise_normalize_p",hi->noise_normalize_p);
  kv_int("coupling_p",hi->coupling_p);
  kv_f64("stereo_point_setting",hi->stereo_point_setting);
  kv_f64("lowpass_kHz",hi->lowpass_kHz); kv_int("lowpass_altered",hi->lowpass_altered);
  kv_f64("ath_floating_dB",hi->ath_floating_dB); kv_f64("ath_absolute_dB",hi->ath_absolute_dB);
  kv_f64("amplitude_track_dBpersec",hi->amplitude_track_dBpersec);
  kv_f64("trigger_setting",hi->trigger_setting);
  kv_f32("pre_amplitude",hi->pre_amplitude);
  arr_begin("block");
  for(int i=0;i<4;i++){
    obj_begin(NULL);
    kv_f64("tone_mask_setting",hi->block[i].tone_mask_setting);
    kv_f64("tone_peaklimit_setting",hi->block[i].tone_peaklimit_setting);
    kv_f64("noise_bias_setting",hi->block[i].noise_bias_setting);
    kv_f64("noise_compand_setting",hi->block[i].noise_compand_setting);
    obj_end();
  }
  arr_end();
  obj_end();

  if(look){
    vorbis_dsp_state vd; vorbis_analysis_init(&vd,&vi);
    private_state *b=vd.backend_state;
    obj_begin("look");
    kv_ints("window_type",b->window,2);
    kv_int("modebits",b->modebits);
    arr_begin("window");
    for(int w=0;w<2;w++){
      kv_f32s(NULL,_vorbis_window_get(b->window[w]),ci->blocksizes[w]/2);
    }
    arr_end();
    arr_begin("mdct");
    for(int w=0;w<2;w++)mdct_look(NULL,(mdct_lookup *)b->transform[w][0]);
    arr_end();
    arr_begin("drft");
    for(int w=0;w<2;w++){
      drft_lookup *d=&b->fft_look[w];
      obj_begin(NULL);
      kv_int("n",d->n);
      kv_f32s("trigcache",d->trigcache,3*d->n);
      kv_ints("splitcache",d->splitcache,32);
      obj_end();
    }
    arr_end();
    {
      envelope_lookup *e=b->ve;
      obj_begin("envelope");
      kv_int("ch",e->ch); kv_int("winlength",e->winlength); kv_int("searchstep",e->searchstep);
      kv_f32("minenergy",e->minenergy); kv_int("storage",e->storage); kv_int("cursor",e->cursor);
      mdct_look("mdct",&e->mdct);
      kv_f32s("mdct_win",e->mdct_win,e->winlength);
      arr_begin("band");
      for(int j=0;j<VE_BANDS;j++){
        obj_begin(NULL);
        kv_int("begin",e->band[j].begin); kv_int("end",e->band[j].end);
        kv_f32s("window",e->band[j].window,e->band[j].end);
        kv_f32("total",e->band[j].total);
        obj_end();
      }
      arr_end();
      obj_end();
    }
    arr_begin("floor");
    for(int i=0;i<ci->floors;i++){
      vorbis_look_floor1 *fl=(vorbis_look_floor1 *)b->flr[i];
      obj_begin(NULL);
      kv_int("posts",fl->posts); kv_int("n",fl->n); kv_int("quant_q",fl->quant_q);
      kv_ints("sorted_index",fl->sorted_index,fl->posts);
      kv_ints("forward_index",fl->forward_index,fl->posts);
      kv_ints("reverse_index",fl->reverse_index,fl->posts);
      kv_ints("hineighbor",fl->hineighbor,fl->posts-2);
      kv_ints("loneighbor",fl->loneighbor,fl->posts-2);
      obj_end();
    }
    arr_end();
    arr_begin("psy");
    for(int i=0;i<ci->psys;i++){
      vorbis_look_psy *p=&b->psy[i];
      int n=p->n;
      obj_begin(NULL);
      kv_int("n",n); kv_int("firstoc",p->firstoc); kv_int("shiftoc",p->shiftoc);
      kv_int("eighth_octave_lines",p->eighth_octave_lines); kv_int("total_octave_lines",p->total_octave_lines);
      kv_int("rate",p->rate);
      kv_ints("m3n",p->m3n,4); kv_f32("m_val",p->m_val);
      kv_int("tonecomp_endp",p->tonecomp_endp); kv_f32("tonecomp_thres",p->tonecomp_thres);
      kv_int("min_nn_lp",p->min_nn_lp); kv_int("endeg_start",p->endeg_start); kv_int("endeg_end",p->endeg_end);
      kv_int("tonefix_end",p->tonefix_end);
      kv_int("n25p",p->n25p); kv_int("n33p",p->n33p); kv_int("n75p",p->n75p);
      kv_int("nn75pt",p->nn75pt); kv_int("nn50pt",p->nn50pt); kv_int("nn25pt",p->nn25pt);
      kv_f32s("ath",p->ath,n);
      kv_longs("octave",p->octave,n);
      kv_longs("bark",p->bark,n);
      arr_begin("noiseoffset"); for(int j=0;j<P_NOISECURVES;j++)kv_f32s(NULL,p->noiseoffset[j],n); arr_end();
      kv_f32s("ntfix_noiseoffset",p->ntfix_noiseoffset,n);
      arr_begin("tonecurves");
      for(int j=0;j<P_BANDS;j++){
        arr_begin(NULL);
        for(int l=0;l<P_LEVELS;l++)kv_f32s(NULL,p->tonecurves[j][l],EHMER_MAX+2);
        arr_end();
      }
      arr_end();
      obj_end();
    }
    arr_end();
    obj_end();
    vorbis_dsp_clear(&vd);
  }

  depth=0; indent(); putchar('}'); putchar('\n');
  vorbis_info_clear(&vi);
  return 0;
}
