use super::{ContentIndex, EventDatabase, MapTable, MultiMapTable, SequenceIndex};
use rkv::{
    backend::{BackendDatabase, BackendEnvironment},
    value::Type,
    DataError, Manager, MultiStore, Rkv, SingleStore, StoreError, Value,
};
use std::convert::TryFrom;
use std::sync::{Arc, RwLock};

pub struct LMDBTable<D, E> {
    store: SingleStore<D>,
    env: Arc<RwLock<Rkv<E>>>,
}

impl<D, E> LMDBTable<D, E> {
    fn new(store: SingleStore<D>, env: Arc<RwLock<Rkv<E>>>) -> LMDBTable<D, E> {
        LMDBTable { store, env }
    }
}

impl<'m, K, V, D, E> MapTable<K, V> for LMDBTable<D, E>
where
    K: AsRef<[u8]>,
    V: TryFrom<&'m [u8]>,
    D: BackendDatabase,
    E: BackendEnvironment<'m>,
{
    type Error = StoreError;

    fn put(&self, key: &K, value: V) -> Result<(), Self::Error> {
        let mut writer = self.env.read()?.write()?;
        self.store
            .put(&mut writer, key, &Value::Blob(value.as_ref()))?;
        writer.commit()
    }

    fn set(&self, key: &K, value: V) -> Result<(), Self::Error> {
        let mut writer = self.env.read()?.write()?;
        self.store.delete(&mut writer, key)?;
        self.store
            .put(&mut writer, key, &Value::Blob(value.as_ref()))?;
        writer.commit()
    }

    fn get(&self, key: &K) -> Result<Option<V>, Self::Error> {
        let reader = self.env.read()?.read()?;
        self.store.get(&reader, key)?.map(|v| match v {
            Value::Blob(b) => V::try_from(b).map_error(|e| DataError::DecodingError {
                value_type: Type::Blob,
                err: ErrorKind,
            }),
            _ => StoreError::DataError(DataError::UnexpectedType {
                expected: Type::Blob,
                actual: Type::from_tag(v.tag),
            }),
        })
    }

    fn del(&self, key: &K) -> Result<(), Self::Error> {
        todo!()
    }
}

pub struct LMDBMultiTable<D, E> {
    store: MultiStore<D>,
    env: Arc<RwLock<Rkv<E>>>,
}

impl<D, E> LMDBMultiTable<D, E> {
    fn new(store: MultiStore<D>, env: Arc<RwLock<Rkv<E>>>) -> LMDBMultiTable<D, E> {
        LMDBMultiTable { store, env }
    }
}

impl<'m, K, V, D, E> MultiMapTable<K, V> for LMDBMultiTable<D, E>
where
    K: AsRef<[u8]>,
    V: AsRef<[u8]>,
    D: BackendDatabase,
    E: BackendEnvironment<'m>,
{
    type Error = StoreError;

    fn put(&self, key: &K, value: V) -> Result<(), Self::Error> {
        let mut writer = self.env.read()?.write()?;
        self.store
            .put(&mut writer, key, &Value::Blob(value.as_ref()))?;
        writer.commit()
    }

    fn set(&self, key: &K, value: V) -> Result<(), Self::Error> {
        let mut writer = self.env.read()?.write()?;
        self.store.delete(&mut writer, key)?;
        self.store
            .put(&mut writer, key, &Value::Blob(value.as_ref()))?;
        writer.commit()
    }

    fn get(&self, key: &K) -> Result<Option<Vec<V>>, Self::Error> {
        let reader = self.env.read()?.read()?;
        Ok(self.itr(key)?.map(|i| i.collect()))
    }

    fn itr(&self, key: &K) -> Result<Option<Box<dyn Iterator<Item = V>>>, Self::Error> {
        let reader = self.env.read()?.read()?;
        self.store.get(&reader, key)?.map(|i| {
            i.map(|v| match v {
                Value::Blob(b) => V::try_from(b).map_error(|e| DataError::DecodingError {
                    value_type: Type::Blob,
                    err: ErrorKind,
                }),
                _ => StoreError::DataError(DataError::UnexpectedType {
                    expected: Type::Blob,
                    actual: Type::from_tag(v.tag),
                }),
            })
        })
    }

    fn del(&self, key: &K) -> Result<(), Self::Error> {
        todo!()
    }
}
