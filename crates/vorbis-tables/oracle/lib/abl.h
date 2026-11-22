/* 調査用のアブレーション（KURA_ABL=M1,M3,... で切る）。本番のコードではない。
   KURA_PORT（照合用のエンコーダ）：環境変数 KURA_ABL がないときの既定を、移植で移さない機能の組
   「PREAMP」（前増幅だけを切る。ADR-149 の改訂：M2・M3・M5・M7・M8・M9 も移す）にする。KURA_ABL があればそれが優先する
   （KURA_ABL= と空にすると aoTuV の機能をすべて入れた状態、KURA_ABL=M2,M3,M5,M7,M8,M9,PREAMP で以前の A′）。 */
#ifndef KURA_ABL_H
#define KURA_ABL_H
#include <stdlib.h>
#include <string.h>
enum { ABL_M1=1<<0, ABL_M2=1<<1, ABL_M3=1<<2, ABL_M4=1<<3, ABL_M5=1<<4, ABL_M6=1<<5, ABL_M7=1<<6,
       ABL_M8=1<<7, ABL_M9=1<<8, ABL_ENV=1<<9, ABL_PREAMP=1<<10, ABL_HYPOT=1<<11, ABL_ATH=1<<12,
       ABL_NN=1<<13, ABL_NOTONE=1<<14, ABL_NOPOINT=1<<15, ABL_LONGONLY=1<<16, ABL_NOEXTRAP=1<<17,
       ABL_ALLPOINT=1<<18, ABL_NOCOMPAND=1<<19 };
#ifdef KURA_PORT
#define KURA_ABL_DEFAULT "PREAMP"
#endif
static inline int kura_abl(void){
  static int v=-1;
  if(v<0){
    const char *names[]={"M1","M2","M3","M4","M5","M6","M7","M8","M9","ENV","PREAMP","HYPOT","ATH","NN","NOTONE","NOPOINT","LONGONLY","NOEXTRAP","ALLPOINT","NOCOMPAND",0};
    const char *e=getenv("KURA_ABL"); v=0;
#ifdef KURA_PORT
    if(!e) e=KURA_ABL_DEFAULT;
#endif
    if(e){ char buf[512]; strncpy(buf,e,511); buf[511]=0; char *t=strtok(buf,",");
      while(t){ for(int i=0;names[i];i++) if(!strcmp(t,names[i])) v|=1<<i;
        if(!strcmp(t,"AOTUV")) v|=0x1fff; /* M1..M9, ENV, PREAMP, HYPOT, ATH */
        t=strtok(0,","); } }
  }
  return v;
}
#define ABL(x) (kura_abl()&ABL_##x)
#endif
