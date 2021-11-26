use domain::base::serial::Serial;


//------------ Version -------------------------------------------------------

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, PartialOrd)]
pub struct Version(Serial);

impl Version {
    pub fn next(self) -> Version {
        Version(self.0.add(1))
    }
}

impl Default for Version {
    fn default() -> Self {
        Version(0.into())
    }
}


//------------ Versioned -----------------------------------------------------

pub struct Versioned<T> {
    data: Vec<(Version, T)>,
}

impl<T> Versioned<T> {
    pub fn new() -> Self {
        Versioned {
            data: Vec::new()
        }
    }

    pub fn get(&self, version: Version) -> Option<&T> {
        self.data.iter().rev().find_map(|item| {
            if item.0 <= version {
                Some(&item.1)
            }
            else {
                None
            }
        })
    }

    pub fn last(&self) -> Option<&T> {
        self.data.last().map(|item| &item.1)
    }

    pub fn last_mut(&mut self) -> Option<&mut T> {
        self.data.last_mut().map(|item| &mut item.1)
    }

    pub fn update(&mut self, version: Version, value: T) {
        if let Some(last) = self.data.last_mut() {
            if last.0 == version {
                last.1 = value;
                return
            }
        }
        self.data.push((version, value))
    }

    /// Drops the last version if it is `version`.
    pub fn rollback(&mut self, version: Version) {
        if self.data.last().map(|item| item.0) == Some(version) {
            self.data.pop();
        }
    }

    pub fn clean(&mut self, version: Version) {
        self.data.retain(|item| item.0 >= version)
    }
}

impl<T> Default for Versioned<T> {
    fn default() -> Self {
        Self::new()
    }
}

