// C のターゲットに 1 つは要るソース。型と関数のプロトタイプのヘッダ（include/kura_mac_ffi.h。関数の定義は Swift の
// @_cdecl）を Swift に渡すために置き、構造体の大きさを確かめる。
#include "kura_mac_ffi.h"

_Static_assert(sizeof(kura_image_info) == 48, "kura_image_info の大きさ");
_Static_assert(sizeof(kura_image_frame) == 24, "kura_image_frame の大きさ");
_Static_assert(sizeof(kura_audio_info) == 48, "kura_audio_info の大きさ");
_Static_assert(sizeof(kura_aac_settings) == 32, "kura_aac_settings の大きさ");
