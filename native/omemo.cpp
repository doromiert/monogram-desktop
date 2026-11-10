// SPDX-License-Identifier: GPL-3.0-or-later
// Native adapter ported from Monogram mobile's omemo2_jni.cpp; no JVM or new ratchet.
#include <openssl/crypto.h>
#include <climits>
#include <cstring>
#include "signal_protocol.h"
#include "curve.h"
#include "ratchet.h"
#include "key_helper.h"
#include "session_builder.h"
#include "session_cipher.h"
#include "protocol.h"
extern "C" int monogram_crypto(signal_context *ctx);
extern "C" {
struct MgStore {
    void *data;
    int (*load)(void *, int, uint32_t, signal_buffer **);
    int (*save)(void *, int, uint32_t, const uint8_t *, size_t);
};
signal_buffer *mg_buffer_new(const uint8_t *p, size_t n) { return signal_buffer_create(p,n); }
const uint8_t *mg_buffer_data(signal_buffer *p) { return signal_buffer_data(p); }
size_t mg_buffer_len(signal_buffer *p) { return signal_buffer_len(p); }
void mg_buffer_free(signal_buffer *p) { signal_buffer_bzero_free(p); }
}
namespace {
enum Kind { IDENTITY, PRE_KEY, SIGNED_PRE_KEY, SESSION, SESSION_IDENTITY, TRUSTED };
struct Error { int code; };
void check(int r) { if(r<0) throw Error{r}; }
void require(bool valid) { if(!valid) throw Error{SG_ERR_INVAL}; }
struct Buffer {
    signal_buffer *p=nullptr;
    ~Buffer() { signal_buffer_bzero_free(p); }
    signal_buffer *release() { auto *v=p;p=nullptr;return v; }
    Buffer()=default;Buffer(const Buffer&)=delete;
};
template<typename T> struct Ref {
    T *p=nullptr;
    ~Ref() { if(p) signal_type_unref(reinterpret_cast<signal_type_base *>(p)); }
    Ref()=default;Ref(const Ref&)=delete;
};
template<typename F> int operation(F f) {
    try { f();return 0; } catch(const Error &e) { return e.code; } catch(...) { return SG_ERR_UNKNOWN; }
}
struct Context {
    signal_context *global=nullptr;
    signal_protocol_store_context *store=nullptr;
    const MgStore *records=nullptr;
    uint32_t local=0;
    bool legacy=false;
    uint8_t version() const { return legacy ? CIPHERTEXT_CURRENT_VERSION : CIPHERTEXT_OMEMO_VERSION; }
    ~Context() { if(store) signal_protocol_store_context_destroy(store);if(global) signal_context_destroy(global); }
    int read(int kind,uint32_t id,signal_buffer **out) {
        *out=nullptr;
        if(!records) return SG_ERR_INVAL;
        int r=records->load(records->data,kind,id,out);
        if(r>0 && (!*out || signal_buffer_len(*out)==0 || signal_buffer_len(*out)>1024*1024)) return SG_ERR_INVAL;
        return r;
    }
    int write(int kind,uint32_t id,const uint8_t *data,size_t len) {
        if(!records || len>1024*1024) return SG_ERR_INVAL;
        return records->save(records->data,kind,id,data,len);
    }
    void init(const MgStore *r=nullptr,uint32_t device=0,bool use_legacy=false);
};
Context *context(void *p) { return static_cast<Context *>(p); }
int loadSession(signal_buffer **out,signal_buffer **user,const signal_protocol_address *a,void *p) { *user=nullptr;return context(p)->read(SESSION,a->device_id,out); }
int saveSession(const signal_protocol_address *a,uint8_t *data,size_t len,uint8_t *,size_t,void *p) { return context(p)->write(SESSION,a->device_id,data,len); }
int containsSession(const signal_protocol_address *a,void *p) { Buffer b;return context(p)->read(SESSION,a->device_id,&b.p); }
int deleteSession(const signal_protocol_address *a,void *p) { return context(p)->write(SESSION,a->device_id,nullptr,0); }
int noDevices(signal_int_list **,const char *,size_t,void *) { return SG_ERR_INVAL; }
int noDeleteAll(const char *,size_t,void *) { return SG_ERR_INVAL; }
int loadPre(signal_buffer **out,uint32_t id,void *p) { int r=context(p)->read(PRE_KEY,id,out);return r==0?SG_ERR_INVALID_KEY_ID:r<0?r:0; }
int savePre(uint32_t id,uint8_t *data,size_t len,void *p) { return context(p)->write(PRE_KEY,id,data,len); }
int containsPre(uint32_t id,void *p) { Buffer b;return context(p)->read(PRE_KEY,id,&b.p); }
int removePre(uint32_t id,void *p) { return context(p)->write(PRE_KEY,id,nullptr,0); }
int loadSigned(signal_buffer **out,uint32_t id,void *p) { int r=context(p)->read(SIGNED_PRE_KEY,id,out);return r==0?SG_ERR_INVALID_KEY_ID:r<0?r:0; }
int saveSigned(uint32_t id,uint8_t *data,size_t len,void *p) { return context(p)->write(SIGNED_PRE_KEY,id,data,len); }
int containsSigned(uint32_t id,void *p) { Buffer b;return context(p)->read(SIGNED_PRE_KEY,id,&b.p); }
int removeSigned(uint32_t id,void *p) { return context(p)->write(SIGNED_PRE_KEY,id,nullptr,0); }
int identityPair(signal_buffer **pub,signal_buffer **priv,void *p) {
    auto *ctx=context(p);Buffer b;Ref<ratchet_identity_key_pair> pair;
    int r=ctx->read(IDENTITY,0,&b.p);if(r<=0) return r<0?r:SG_ERR_INVALID_KEY;
    r=ratchet_identity_key_pair_deserialize(&pair.p,signal_buffer_data(b.p),signal_buffer_len(b.p),ctx->global);if(r<0)return r;
    r=ec_public_key_serialize(pub,ratchet_identity_key_pair_get_public(pair.p));if(r<0)return r;
    return ec_private_key_serialize(priv,ratchet_identity_key_pair_get_private(pair.p));
}
int registration(void *p,uint32_t *id) { *id=context(p)->local;return 0; }
int trusted(const signal_protocol_address *a,uint8_t *key,size_t len,void *p) {
    auto *ctx=context(p);Buffer pin,mont;Ref<ec_public_key> identity;
    int r=ctx->read(TRUSTED,a->device_id,&pin.p);if(r<=0)return r;
    if(signal_buffer_len(pin.p)!=32)return SG_ERR_INVAL;
    r=ctx->legacy ? curve_decode_point_mont(&identity.p,signal_buffer_data(pin.p),32,ctx->global)
                  : curve_decode_point_ed(&identity.p,signal_buffer_data(pin.p),32,ctx->global);
    if(r<0)return r;
    r=ec_public_key_serialize(&mont.p,identity.p);if(r<0)return r;
    return signal_buffer_len(mont.p)==len && CRYPTO_memcmp(signal_buffer_data(mont.p),key,len)==0;
}
int observe(const signal_protocol_address *a,uint8_t *key,size_t len,void *p) { int r=trusted(a,key,len,p);return r==1?0:r<0?r:SG_ERR_UNTRUSTED_IDENTITY; }
void Context::init(const MgStore *r,uint32_t device,bool use_legacy) {
    legacy=use_legacy;
    check(signal_context_create(&global,nullptr));check(monogram_crypto(global));
    if(!r)return;
    require(device>0 && device<=INT_MAX && r->load && r->save);records=r;local=device;
    check(signal_protocol_store_context_create(&store,global));
    signal_protocol_session_store sessions={loadSession,noDevices,saveSession,containsSession,deleteSession,noDeleteAll,nullptr,this};
    signal_protocol_pre_key_store prekeys={loadPre,savePre,containsPre,removePre,nullptr,this};
    signal_protocol_signed_pre_key_store signedKeys={loadSigned,saveSigned,containsSigned,removeSigned,nullptr,this};
    signal_protocol_identity_key_store identities={identityPair,registration,observe,trusted,nullptr,this};
    check(signal_protocol_store_context_set_session_store(store,&sessions));
    check(signal_protocol_store_context_set_pre_key_store(store,&prekeys));
    check(signal_protocol_store_context_set_signed_pre_key_store(store,&signedKeys));
    check(signal_protocol_store_context_set_identity_key_store(store,&identities));
}
signal_protocol_address address(const char *peer,size_t len,uint32_t device) { require(peer && len>0 && len<=4096 && device>0 && device<=INT_MAX);return {peer,len,(int32_t)device}; }
void pinned(Context &ctx,uint32_t device,const uint8_t *key,size_t len) {
    Buffer pin;check(ctx.read(TRUSTED,device,&pin.p));
    if(!pin.p || len!=32 || signal_buffer_len(pin.p)!=32 || CRYPTO_memcmp(signal_buffer_data(pin.p),key,32)) throw Error{SG_ERR_UNTRUSTED_IDENTITY};
}
void existingIdentity(Context &ctx,uint32_t device) { Buffer b;check(ctx.read(SESSION_IDENTITY,device,&b.p));if(!b.p)throw Error{SG_ERR_UNTRUSTED_IDENTITY};pinned(ctx,device,signal_buffer_data(b.p),signal_buffer_len(b.p)); }
}
extern "C" {
int mg_identity(uint8_t legacy,signal_buffer **record,signal_buffer **publicKey) {
    *record=nullptr;*publicKey=nullptr;
    return operation([&] { Context ctx;ctx.init();Ref<ratchet_identity_key_pair> pair;Buffer r,p;
        check(signal_protocol_key_helper_generate_identity_key_pair(&pair.p,ctx.global));
        check(ratchet_identity_key_pair_serialize(&r.p,pair.p));
        auto *identity=ratchet_identity_key_pair_get_public(pair.p);
        if(legacy) check(ec_public_key_serialize_omemo(&p.p,identity));
        else { p.p=ec_public_key_get_ed(identity);require(p.p); }
        *record=r.release();*publicKey=p.release(); });
}
int mg_pre_key(uint32_t id,signal_buffer **record,signal_buffer **publicKey) {
    *record=nullptr;*publicKey=nullptr;
    return operation([&] { require(id>0 && id<=INT_MAX);Context ctx;ctx.init();Ref<ec_key_pair> pair;Ref<session_pre_key> key;Buffer r,p;
        check(curve_generate_key_pair(ctx.global,&pair.p));check(session_pre_key_create(&key.p,id,pair.p));
        check(session_pre_key_serialize(&r.p,key.p));check(ec_public_key_serialize_omemo(&p.p,ec_key_pair_get_public(pair.p)));
        *record=r.release();*publicKey=p.release(); });
}
int mg_signed_key(uint8_t legacy,const uint8_t *identity,size_t length,uint32_t id,uint64_t time,signal_buffer **record,signal_buffer **publicKey,signal_buffer **signature) {
    *record=nullptr;*publicKey=nullptr;*signature=nullptr;
    return operation([&] { require(identity && length>0 && length<=1024*1024 && id>0 && id<=INT_MAX);Context ctx;ctx.init();Ref<ratchet_identity_key_pair> pair;Ref<session_signed_pre_key> key;Buffer r,p,s;
        check(ratchet_identity_key_pair_deserialize(&pair.p,identity,length,ctx.global));
        check(signal_protocol_key_helper_generate_signed_pre_key(&key.p,pair.p,id,time,ctx.global));
        check(session_signed_pre_key_serialize(&r.p,key.p));check(ec_public_key_serialize_omemo(&p.p,ec_key_pair_get_public(session_signed_pre_key_get_key_pair(key.p))));
        s.p=legacy ? signal_buffer_create(session_signed_pre_key_get_signature(key.p),session_signed_pre_key_get_signature_len(key.p))
                   : signal_buffer_create(session_signed_pre_key_get_signature_omemo(key.p),session_signed_pre_key_get_signature_omemo_len(key.p));
        require(s.p);
        *record=r.release();*publicKey=p.release();*signature=s.release(); });
}
int mg_verify_legacy(const uint8_t *identity,const uint8_t *key,const uint8_t *signature) {
    return operation([&] { require(identity && key && signature);Context ctx;ctx.init();Ref<ec_public_key> ik,spk;Buffer serialized;
        check(curve_decode_point_mont(&ik.p,identity,32,ctx.global));check(curve_decode_point_mont(&spk.p,key,32,ctx.global));
        check(ec_public_key_serialize(&serialized.p,spk.p));
        int valid=curve_verify_signature(ik.p,signal_buffer_data(serialized.p),signal_buffer_len(serialized.p),signature,64);
        check(valid);require(valid==1); });
}
int mg_build(uint8_t legacy,const MgStore *store,uint32_t local,const char *peer,size_t peer_len,uint32_t device,uint32_t pre_id,const uint8_t *pre,uint32_t signed_id,const uint8_t *signed_key,const uint8_t *signature,const uint8_t *identity) {
    return operation([&] { require(pre && signed_key && signature && identity);Context ctx;ctx.init(store,local,legacy);auto a=address(peer,peer_len,device);pinned(ctx,device,identity,32);if(!legacy)require((signature[63]&0xe0)==0);
        Ref<ec_public_key> p,s,ik;Ref<session_pre_key_bundle> bundle;
        check(curve_decode_point_mont(&p.p,pre,32,ctx.global));check(curve_decode_point_mont(&s.p,signed_key,32,ctx.global));check(legacy ? curve_decode_point_mont(&ik.p,identity,32,ctx.global) : curve_decode_point_ed(&ik.p,identity,32,ctx.global));
        check(session_pre_key_bundle_create(&bundle.p,device,device,pre_id,p.p,signed_id,s.p,signature,64,ik.p));
        session_builder *builder=nullptr;check(session_builder_create(&builder,ctx.store,&a,ctx.global));session_builder_set_version(builder,ctx.version());
        int result=session_builder_process_pre_key_bundle(builder,bundle.p);session_builder_free(builder);check(result);
        check(ctx.write(SESSION_IDENTITY,device,identity,32)); });
}
int mg_encrypt(uint8_t legacy,const MgStore *store,uint32_t local,const char *peer,size_t peer_len,uint32_t device,const uint8_t *plain,size_t length,uint8_t *pre_key,signal_buffer **out) {
    *out=nullptr;
    return operation([&] { require(plain && length>0 && length<=1024*1024);Context ctx;ctx.init(store,local,legacy);auto a=address(peer,peer_len,device);existingIdentity(ctx,device);Ref<ciphertext_message> encrypted;
        session_cipher *cipher=nullptr;check(session_cipher_create(&cipher,ctx.store,&a,ctx.global));session_cipher_set_version(cipher,ctx.version());
        int result=session_cipher_encrypt(cipher,plain,length,&encrypted.p);session_cipher_free(cipher);check(result);
        *pre_key=ciphertext_message_get_type(encrypted.p)==CIPHERTEXT_PREKEY_TYPE;
        *out=signal_buffer_copy(ciphertext_message_get_serialized(encrypted.p));require(*out); });
}
int mg_decrypt(uint8_t legacy,const MgStore *store,uint32_t local,const char *peer,size_t peer_len,uint32_t device,const uint8_t *bytes,size_t length,uint8_t pre_key,signal_buffer **out) {
    *out=nullptr;
    return operation([&] { require(bytes && length>0 && length<=1024*1024 && pre_key<=1);Context ctx;ctx.init(store,local,legacy);auto a=address(peer,peer_len,device);Buffer plain,identity;Ref<pre_key_signal_message> first;Ref<signal_message> message;
        if(pre_key) {
            check(legacy ? pre_key_signal_message_deserialize(&first.p,bytes,length,ctx.global)
                         : pre_key_signal_message_deserialize_omemo(&first.p,bytes,length,device,ctx.global));
            require(pre_key_signal_message_get_message_version(first.p)==ctx.version());
            auto *public_key=pre_key_signal_message_get_identity_key(first.p);
            if(legacy) check(ec_public_key_serialize_omemo(&identity.p,public_key));
            else { identity.p=ec_public_key_get_ed(public_key);require(identity.p); }
            pinned(ctx,device,signal_buffer_data(identity.p),signal_buffer_len(identity.p));
        } else {
            existingIdentity(ctx,device);
            check(legacy ? signal_message_deserialize(&message.p,bytes,length,ctx.global)
                         : signal_message_deserialize_omemo(&message.p,bytes,length,ctx.global));
            require(signal_message_get_message_version(message.p)==ctx.version());
        }
        session_cipher *cipher=nullptr;check(session_cipher_create(&cipher,ctx.store,&a,ctx.global));session_cipher_set_version(cipher,ctx.version());
        int result=pre_key?session_cipher_decrypt_pre_key_signal_message(cipher,first.p,nullptr,&plain.p):session_cipher_decrypt_signal_message(cipher,message.p,nullptr,&plain.p);
        session_cipher_free(cipher);check(result);
        if(identity.p)check(ctx.write(SESSION_IDENTITY,device,signal_buffer_data(identity.p),signal_buffer_len(identity.p)));
        *out=plain.release(); });
}
}
